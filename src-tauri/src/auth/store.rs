use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct StoredSession {
    pub token: String,
    pub expires_at: String,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct AuthStore {
    pub sessions: HashMap<String, StoredSession>,
}

fn auth_store_path() -> PathBuf {
    dirs_next::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("prisma-desktop")
        .join("auth.json")
}

pub fn load_auth_store() -> AuthStore {
    let path = auth_store_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_auth_store(store: &AuthStore) {
    let path = auth_store_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(json) = serde_json::to_string_pretty(store) else { return };
    if std::fs::write(&path, &json).is_err() {
        return;
    }
    // Kept as its own file (not folded into settings.json) and
    // 0600-permissioned — it holds bearer tokens, not just window
    // geometry. A real OS keyring is skipped deliberately: this project
    // explicitly targets WSL2, which has no reliable secret-service to
    // back one.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o600);
            let _ = std::fs::set_permissions(&path, perms);
        }
    }
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
