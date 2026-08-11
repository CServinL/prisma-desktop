//! Generic "load/save a JSON file under the OS config dir" helper —
//! consolidates what used to be three independently drifting copies
//! (settings.rs, auth/store.rs, sync/mod.rs). Only auth/store.rs's copy
//! additionally chmod'd the file to 0600 (it holds bearer tokens, not just
//! window geometry) — `restrict_perms` makes that an explicit, visible
//! choice at each call site instead of something only one of three copies
//! happened to do, easy to lose track of if a fourth secrets-bearing file
//! were ever added by copying the wrong one.
//!
//! `load_json` is version-aware (2026-08-06, schema_gov::migration wired
//! in): a missing file is the expected first-run case and silently defaults,
//! but a file that exists and fails to parse, migrate, or match the current
//! shape after migration now logs a warning before defaulting — previously
//! (`.ok().unwrap_or_default()`) any of those three failure modes silently
//! discarded the file with zero trace, which for auth.json meant a silent,
//! unexplained logout on anything from a real corruption to a future field
//! rename with no migration registered for it.

use std::path::{Path, PathBuf};

use serde::{de::DeserializeOwned, Serialize};

use crate::schema_gov::MigrationChain;

/// `PRISMA_DESKTOP_CONFIG_DIR` overrides the OS config dir when set --
/// exists so tests can redirect settings/auth-store/sync-state I/O at a
/// tmp dir instead of silently reading/writing the real
/// `~/.config/prisma-desktop/` on whatever machine runs `cargo test`.
/// Nothing set this before resolve_conflict_push_side (via save_sync_state)
/// got its first direct test -- every prior test either avoided these
/// functions entirely or constructed a SyncContext by hand without going
/// through load/save at all.
pub fn config_file_path(filename: &str) -> PathBuf {
    let base = std::env::var_os("PRISMA_DESKTOP_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(dirs_next::config_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("prisma-desktop").join(filename)
}

/// Serializes any test that sets PRISMA_DESKTOP_CONFIG_DIR against every
/// other such test -- std::env::set_var mutates whole-process state, and
/// cargo test runs tests in parallel by default, so two tests setting this
/// concurrently to different tmp dirs would race on which one "wins."
/// Unrelated tests that never touch this env var are unaffected.
#[cfg(test)]
pub(crate) static TEST_ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn load_json<T: DeserializeOwned + Default>(path: &Path, chain: &MigrationChain) -> T {
    let raw = match std::fs::read_to_string(path) {
        // No file yet is the expected first-run state, not a failure --
        // nothing to warn about.
        Err(_) => return T::default(),
        Ok(s) => s,
    };
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("prisma-desktop: {} is not valid JSON ({e}) -- resetting to defaults", path.display());
            return T::default();
        }
    };
    let migrated = match chain.migrate(value) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("prisma-desktop: failed to migrate {} ({e}) -- resetting to defaults", path.display());
            return T::default();
        }
    };
    match serde_json::from_value(migrated) {
        Ok(t) => t,
        Err(e) => {
            eprintln!(
                "prisma-desktop: {} migrated but no longer matches the current shape ({e}) -- resetting to defaults",
                path.display(),
            );
            T::default()
        }
    }
}

pub fn save_json<T: Serialize>(path: &Path, value: &T, restrict_perms: bool) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(json) = serde_json::to_string_pretty(value) else { return };
    if std::fs::write(path, &json).is_err() {
        return;
    }
    if restrict_perms {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(path) {
                let mut perms = meta.permissions();
                perms.set_mode(0o600);
                let _ = std::fs::set_permissions(path, perms);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::collections::HashMap;

    #[derive(Serialize, Deserialize, Default, PartialEq, Debug)]
    struct Sample {
        value: u32,
    }

    fn no_op_chain() -> MigrationChain {
        MigrationChain { current_version: 1, migrations: HashMap::new() }
    }

    #[test]
    fn load_json_returns_default_when_file_missing() {
        let path = std::env::temp_dir().join(format!("prisma-desktop-test-{}-missing.json", uuid::Uuid::new_v4()));
        let loaded: Sample = load_json(&path, &no_op_chain());
        assert_eq!(loaded, Sample::default());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let path = std::env::temp_dir().join(format!("prisma-desktop-test-{}.json", uuid::Uuid::new_v4()));
        save_json(&path, &Sample { value: 42 }, false);
        let loaded: Sample = load_json(&path, &no_op_chain());
        assert_eq!(loaded, Sample { value: 42 });
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_json_resets_to_default_on_unparseable_json() {
        // Real corruption, not just an old shape -- must still default
        // rather than crash or propagate an error the caller has no UI for,
        // but this path is distinct from "file missing" (worth its own
        // test since the two used to be indistinguishable behavior).
        let path = std::env::temp_dir().join(format!("prisma-desktop-test-{}-corrupt.json", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"{not valid json").unwrap();
        let loaded: Sample = load_json(&path, &no_op_chain());
        assert_eq!(loaded, Sample::default());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_json_applies_a_registered_migration() {
        fn v1_to_v2(mut raw: serde_json::Value) -> Result<serde_json::Value, String> {
            let obj = raw.as_object_mut().ok_or("expected object")?;
            if let Some(old) = obj.remove("old_name") {
                obj.insert("value".to_string(), old);
            }
            Ok(raw)
        }
        let path = std::env::temp_dir().join(format!("prisma-desktop-test-{}-migrate.json", uuid::Uuid::new_v4()));
        std::fs::write(&path, br#"{"old_name": 7}"#).unwrap();
        let mut migrations: HashMap<u32, crate::schema_gov::migration::MigrationFn> = HashMap::new();
        migrations.insert(1, v1_to_v2);
        let chain = MigrationChain { current_version: 2, migrations };

        let loaded: Sample = load_json(&path, &chain);

        assert_eq!(loaded, Sample { value: 7 });
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_json_resets_to_default_when_no_migration_covers_an_old_version() {
        let path = std::env::temp_dir().join(format!("prisma-desktop-test-{}-unmigratable.json", uuid::Uuid::new_v4()));
        std::fs::write(&path, br#"{"schema_version": 1, "value": 9}"#).unwrap();
        // current_version 2 but no migration registered from 1 -- migrate()
        // errors, load_json must still default rather than panic/propagate.
        let chain = MigrationChain { current_version: 2, migrations: HashMap::new() };

        let loaded: Sample = load_json(&path, &chain);

        assert_eq!(loaded, Sample::default());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    #[cfg(unix)]
    fn save_json_restricts_perms_when_requested() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("prisma-desktop-test-{}-restricted.json", uuid::Uuid::new_v4()));
        save_json(&path, &Sample { value: 1 }, true);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn config_file_path_honors_env_override() {
        let _guard = TEST_ENV_GUARD.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}-cfgdir", uuid::Uuid::new_v4()));
        std::env::set_var("PRISMA_DESKTOP_CONFIG_DIR", &dir);
        let path = config_file_path("whatever.json");
        std::env::remove_var("PRISMA_DESKTOP_CONFIG_DIR");
        assert_eq!(path, dir.join("prisma-desktop").join("whatever.json"));
    }

}
