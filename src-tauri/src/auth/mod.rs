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
/// existed. This process never stores the password itself -- a stored
/// session that /auth/refresh can no longer slide forward (see
/// check_and_refresh_session below) can only recover via a fresh
/// sync_login call, same as a session that was never established at all.
#[tauri::command]
pub async fn sync_login(app: tauri::AppHandle, server_url: String, password: String) -> Result<LoginResult, String> {
    // reqwest::Client::new() has no request timeout by default -- see
    // sync/mod.rs's build_http_client() for the live incident (2026-08-27)
    // that surfaced this same gap in sync's own client. 5s matches
    // check_and_refresh_session below, the other auth call in this file.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
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

/// Startup auth check, called from window::resolve_start_url before
/// deciding /app vs. the fallback page: no stored session at all -> nothing
/// to verify, caller proceeds as if auth weren't a concern (a mode: none
/// server, or a first-ever run, both fall through here -- the SvelteKit
/// UI's own apiFetch/onAuthRequired handles prompting for a first login
/// once it hits a genuine 401 inside /app). A stored session gets slid
/// forward via POST /auth/refresh rather than trusted blindly -- a session
/// that's actually dead (wrong signing key, e.g. from a password rotation,
/// or a stale entry saved under an earlier URL-formatting convention, see
/// the 2026-08-21 incident this was built for) must not silently load /app
/// only to have every single request inside it 401 one at a time.
#[derive(Debug, PartialEq)]
pub enum AuthCheckOutcome {
    NoSessionStored,
    Refreshed,
    Dead,
}

pub fn check_and_refresh_session(server_url: &str) -> AuthCheckOutcome {
    let Some(session) = store::session_for(server_url) else {
        return AuthCheckOutcome::NoSessionStored;
    };
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(5)).build();
    let Ok(client) = client else { return AuthCheckOutcome::NoSessionStored };
    let result = tauri::async_runtime::block_on(async {
        client
            .post(format!("{}/auth/refresh", server_url.trim_end_matches('/')))
            .bearer_auth(&session.token)
            .send()
            .await
    });
    match result {
        Ok(resp) if resp.status().is_success() => {
            match tauri::async_runtime::block_on(resp.json::<LoginResponse>()) {
                Ok(body) => {
                    store::set_session(
                        server_url,
                        StoredSession { token: body.token, expires_at: body.expires_at },
                    );
                    AuthCheckOutcome::Refreshed
                }
                // Malformed response body -- treat like any other transport
                // hiccup below, not proof the session is dead.
                Err(_) => AuthCheckOutcome::Refreshed,
            }
        }
        Ok(resp) if resp.status().as_u16() == 401 || resp.status().as_u16() == 403 => {
            store::clear_session(server_url);
            AuthCheckOutcome::Dead
        }
        // Any other outcome (network error, 5xx, server doesn't have auth
        // enabled at all) isn't proof the session is dead -- don't bounce a
        // reachable-but-momentarily-flaky server to the login screen.
        _ => AuthCheckOutcome::Refreshed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_isolated_config_dir<T>(f: impl FnOnce() -> T) -> T {
        let _guard = crate::json_store::TEST_ENV_GUARD.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}-authmod", uuid::Uuid::new_v4()));
        std::env::set_var("PRISMA_DESKTOP_CONFIG_DIR", &dir);
        let result = f();
        std::env::remove_var("PRISMA_DESKTOP_CONFIG_DIR");
        std::fs::remove_dir_all(&dir).ok();
        result
    }

    #[test]
    fn check_and_refresh_session_with_nothing_stored_is_a_noop() {
        with_isolated_config_dir(|| {
            let outcome = check_and_refresh_session("http://127.0.0.1:1");
            assert_eq!(outcome, AuthCheckOutcome::NoSessionStored);
        });
    }

    #[test]
    fn check_and_refresh_session_stores_the_new_token_on_success() {
        with_isolated_config_dir(|| {
            let server_url = crate::sync::tests::spawn_mock_response(
                200,
                r#"{"token":"fresh-token","expires_at":"2027-01-01T00:00:00Z"}"#,
            );
            store::set_session(
                &server_url,
                StoredSession { token: "old-token".into(), expires_at: "2026-01-01T00:00:00Z".into() },
            );

            let outcome = check_and_refresh_session(&server_url);

            assert_eq!(outcome, AuthCheckOutcome::Refreshed);
            let stored = store::session_for(&server_url).expect("session should still be stored");
            assert_eq!(stored.token, "fresh-token");
            assert_eq!(stored.expires_at, "2027-01-01T00:00:00Z");
        });
    }

    #[test]
    fn check_and_refresh_session_clears_a_dead_session_on_401() {
        with_isolated_config_dir(|| {
            let server_url = crate::sync::tests::spawn_mock_response(401, "");
            store::set_session(
                &server_url,
                StoredSession { token: "dead-token".into(), expires_at: "2026-01-01T00:00:00Z".into() },
            );

            let outcome = check_and_refresh_session(&server_url);

            assert_eq!(outcome, AuthCheckOutcome::Dead);
            assert!(store::session_for(&server_url).is_none());
        });
    }

    #[test]
    fn check_and_refresh_session_keeps_the_stored_session_on_a_5xx() {
        with_isolated_config_dir(|| {
            let server_url = crate::sync::tests::spawn_mock_response(500, "");
            store::set_session(
                &server_url,
                StoredSession { token: "still-good".into(), expires_at: "2026-01-01T00:00:00Z".into() },
            );

            let outcome = check_and_refresh_session(&server_url);

            assert_eq!(outcome, AuthCheckOutcome::Refreshed);
            let stored = store::session_for(&server_url).expect("a transport/server hiccup must not drop the session");
            assert_eq!(stored.token, "still-good");
        });
    }
}
