use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::schema_gov::MigrationChain;

/// See settings.rs's SETTINGS_SCHEMA_VERSION for the convention this
/// mirrors. No real migration needed yet.
pub const AUTH_STORE_SCHEMA_VERSION: u32 = 1;

fn auth_store_migration_chain() -> MigrationChain {
    MigrationChain { current_version: AUTH_STORE_SCHEMA_VERSION, migrations: HashMap::new() }
}

fn default_schema_version() -> u32 { 1 }

#[derive(Serialize, Deserialize, Clone)]
pub struct StoredSession {
    pub token: String,
    pub expires_at: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct AuthStore {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub sessions: HashMap<String, StoredSession>,
}

impl Default for AuthStore {
    fn default() -> Self {
        Self { schema_version: AUTH_STORE_SCHEMA_VERSION, sessions: HashMap::new() }
    }
}

fn auth_store_path() -> PathBuf {
    crate::json_store::config_file_path("auth.json")
}

pub fn load_auth_store() -> AuthStore {
    crate::json_store::load_json(&auth_store_path(), &auth_store_migration_chain())
}

pub fn save_auth_store(store: &AuthStore) {
    // Kept as its own file (not folded into settings.json) and
    // 0600-permissioned — it holds bearer tokens, not just window
    // geometry. A real OS keyring (libsecret/GNOME Keyring, KWallet via
    // e.g. the `keyring` crate) is skipped for now, not ruled out — WSL2
    // support (dropped 2026-07-30) was the original reason, since WSL2 has
    // no reliable secret-service to back one; a native Linux desktop does.
    crate::json_store::save_json(&auth_store_path(), store, true)
}

pub fn session_for(server_url: &str) -> Option<StoredSession> {
    load_auth_store().sessions.get(server_url).cloned()
}

pub fn set_session(server_url: &str, session: StoredSession) {
    let mut store = load_auth_store();
    store.sessions.insert(server_url.to_string(), session);
    save_auth_store(&store);
}

pub fn clear_session(server_url: &str) {
    let mut store = load_auth_store();
    store.sessions.remove(server_url);
    save_auth_store(&store);
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every test here redirects PRISMA_DESKTOP_CONFIG_DIR to a tmp dir --
    // load_auth_store/save_auth_store otherwise read/write the real
    // ~/.config/prisma-desktop/auth.json on whatever machine runs `cargo
    // test`. Serialized against each other via TEST_ENV_GUARD since
    // std::env::set_var is whole-process state.

    fn with_isolated_config_dir<T>(f: impl FnOnce() -> T) -> T {
        let _guard = crate::json_store::TEST_ENV_GUARD.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}-authcfg", uuid::Uuid::new_v4()));
        std::env::set_var("PRISMA_DESKTOP_CONFIG_DIR", &dir);
        let result = f();
        std::env::remove_var("PRISMA_DESKTOP_CONFIG_DIR");
        std::fs::remove_dir_all(&dir).ok();
        result
    }

    #[test]
    fn load_auth_store_defaults_to_empty_when_file_missing() {
        with_isolated_config_dir(|| {
            let store = load_auth_store();
            assert!(store.sessions.is_empty());
        });
    }

    #[test]
    fn session_for_returns_none_when_no_session_set() {
        with_isolated_config_dir(|| {
            assert!(session_for("http://127.0.0.1:8765").is_none());
        });
    }

    #[test]
    fn set_session_then_session_for_roundtrips() {
        with_isolated_config_dir(|| {
            set_session("http://127.0.0.1:8765", StoredSession {
                token: "tok-123".into(),
                expires_at: "2026-08-01T00:00:00Z".into(),
            });
            let session = session_for("http://127.0.0.1:8765").expect("session should be stored");
            assert_eq!(session.token, "tok-123");
            assert_eq!(session.expires_at, "2026-08-01T00:00:00Z");
        });
    }

    #[test]
    fn set_session_is_keyed_per_server_url() {
        with_isolated_config_dir(|| {
            set_session("http://server-a", StoredSession { token: "a".into(), expires_at: "x".into() });
            set_session("http://server-b", StoredSession { token: "b".into(), expires_at: "x".into() });
            assert_eq!(session_for("http://server-a").unwrap().token, "a");
            assert_eq!(session_for("http://server-b").unwrap().token, "b");
        });
    }

    #[test]
    fn clear_session_removes_only_the_named_server() {
        with_isolated_config_dir(|| {
            set_session("http://server-a", StoredSession { token: "a".into(), expires_at: "x".into() });
            set_session("http://server-b", StoredSession { token: "b".into(), expires_at: "x".into() });
            clear_session("http://server-a");
            assert!(session_for("http://server-a").is_none());
            assert!(session_for("http://server-b").is_some());
        });
    }

    #[test]
    #[cfg(unix)]
    fn save_auth_store_restricts_file_to_0600() {
        use std::os::unix::fs::PermissionsExt;
        with_isolated_config_dir(|| {
            set_session("http://127.0.0.1:8765", StoredSession { token: "tok".into(), expires_at: "x".into() });
            let mode = std::fs::metadata(auth_store_path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "auth.json holds bearer tokens, must not be world/group readable");
        });
    }
}
