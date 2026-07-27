//! Generic "load/save a JSON file under the OS config dir, default on any
//! failure" helper — consolidates what used to be three independently
//! drifting copies (settings.rs, auth/store.rs, sync/mod.rs). Only
//! auth/store.rs's copy additionally chmod'd the file to 0600 (it holds
//! bearer tokens, not just window geometry) — `restrict_perms` makes that
//! an explicit, visible choice at each call site instead of something only
//! one of three copies happened to do, easy to lose track of if a fourth
//! secrets-bearing file were ever added by copying the wrong one.

use std::path::{Path, PathBuf};

use serde::{de::DeserializeOwned, Serialize};

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

pub fn load_json<T: DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
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

    #[derive(Serialize, Deserialize, Default, PartialEq, Debug)]
    struct Sample {
        value: u32,
    }

    #[test]
    fn load_json_returns_default_when_file_missing() {
        let path = std::env::temp_dir().join(format!("prisma-desktop-test-{}-missing.json", uuid::Uuid::new_v4()));
        let loaded: Sample = load_json(&path);
        assert_eq!(loaded, Sample::default());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let path = std::env::temp_dir().join(format!("prisma-desktop-test-{}.json", uuid::Uuid::new_v4()));
        save_json(&path, &Sample { value: 42 }, false);
        let loaded: Sample = load_json(&path);
        assert_eq!(loaded, Sample { value: 42 });
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
