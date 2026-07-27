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
    crate::json_store::config_file_path("auth.json")
}

pub fn load_auth_store() -> AuthStore {
    crate::json_store::load_json(&auth_store_path())
}

pub fn save_auth_store(store: &AuthStore) {
    // Kept as its own file (not folded into settings.json) and
    // 0600-permissioned — it holds bearer tokens, not just window
    // geometry. A real OS keyring is skipped deliberately: this project
    // explicitly targets WSL2, which has no reliable secret-service to
    // back one.
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
