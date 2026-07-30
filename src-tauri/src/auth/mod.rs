mod store;

pub use store::{session_for, StoredSession};

use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct LoginResponse {
    token: String,
    expires_at: String,
}

#[derive(Serialize)]
pub struct LoginResult {
    // Handed back to the frontend (not just persisted in auth.json) so a
    // single password prompt in the Tauri UI serves both the Rust sync
    // engine's own session AND the SvelteKit app's own authFetch calls —
    // see prisma/ui/src/lib/auth.ts.
    pub token: String,
    pub expires_at: String,
}

/// Logs in against `server_url`'s ADR-011 password-mode auth and persists
/// the resulting session in auth.json. A no-op-looking success (mode:
/// none on the server) never reaches this — the server's /auth/login
/// returns 404 in that case, surfaced here as an Err.
///
/// Also hands the fresh token straight to a currently-running sync engine
/// (if one is running against this same server_url) via
/// sync::update_running_token -- without this, re-authenticating here only
/// ever updated the on-disk store, so a connection already stuck in
/// SyncContext::needs_reauth (see sync/pull.rs) stayed stuck until the user
/// separately stopped and restarted sync, even though a valid token now
/// existed. There is no refresh-token concept in this password-mode auth
/// (ADR-011) and this process never stores the password -- a fresh
/// sync_login call is the only way a stuck connection recovers at all.
#[tauri::command]
pub async fn sync_login(app: tauri::AppHandle, server_url: String, password: String) -> Result<LoginResult, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/auth/login", server_url.trim_end_matches('/')))
        .json(&serde_json::json!({ "password": password }))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let status = resp.status();
        let detail = resp.text().await.unwrap_or_default();
        return Err(format!("login failed ({status}): {detail}"));
    }

    let body: LoginResponse = resp.json().await.map_err(|e| e.to_string())?;
    store::set_session(
        &server_url,
        StoredSession { token: body.token.clone(), expires_at: body.expires_at.clone() },
    );
    crate::sync::update_running_token(&app, &server_url, Some(body.token.clone()));
    Ok(LoginResult { token: body.token, expires_at: body.expires_at })
}

/// Also clears a running engine's live token for this server (see
/// sync_login's doc comment) -- otherwise an explicit logout leaves a
/// currently-running connection still authenticated with a token the user
/// just chose to invalidate client-side.
#[tauri::command]
pub fn sync_logout(app: tauri::AppHandle, server_url: String) -> Result<(), String> {
    store::clear_session(&server_url);
    crate::sync::update_running_token(&app, &server_url, None);
    Ok(())
}

#[derive(Serialize)]
pub struct SyncStatus {
    pub logged_in: bool,
    pub expires_at: Option<String>,
    // See LoginResult's token field — same reasoning: lets the frontend
    // pick up an already-stored Rust-side session on startup without
    // reprompting for a password every time the page reloads.
    pub token: Option<String>,
}

#[tauri::command]
pub fn sync_status(server_url: String) -> SyncStatus {
    match store::session_for(&server_url) {
        Some(s) => SyncStatus { logged_in: true, expires_at: Some(s.expires_at), token: Some(s.token) },
        None => SyncStatus { logged_in: false, expires_at: None, token: None },
    }
}
