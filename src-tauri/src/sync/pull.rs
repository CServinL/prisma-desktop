//! Server <-> desktop over one persistent WebSocket connection. As of the
//! 2026-07-26 redesign, the server is the sole sync orchestrator: it asks
//! this client for information (`request_manifest`) and tells it what to do
//! (`push_file`, `request_file`) rather than this client deciding on its own
//! to push whenever its local fs watcher fires. That watcher-driven design
//! caused a real, sustained bug -- a local fs-watcher event is not proof
//! content actually changed, and with two independent sides each free to
//! decide "I should push now," a spurious/duplicate watcher event could
//! loop indefinitely with nothing to arbitrate it. Now there is exactly one
//! decision-maker (the server), and every operation that matters gets an
//! explicit acknowledgement back to whichever side needs to trust it
//! completed -- mirroring a normal network ACK: a push-down isn't
//! considered done by the server until this client confirms it (`file_synced`),
//! a pull-up is already confirmed by the ordinary HTTP response the PUT
//! returns, and a bare notification (`file_changed`/`file_deleted`) gets an
//! explicit `ack` back when the server decides no further action is needed,
//! so this client is never left guessing whether its message arrived.
//!
//! Auth is carried via the `Sec-WebSocket-Protocol` subprotocol (["bearer",
//! "<jwt>"]) rather than a query param, matching the server's
//! AuthMiddleware/_bearer_from_ws (see prisma/server/auth.py) — this keeps
//! the token out of URLs/logs on both sides of the connection.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use super::{SyncContext, TrackedFile};

#[derive(Deserialize)]
struct IncomingMessage {
    #[serde(rename = "type")]
    kind: String,
    action: Option<String>,
    path: Option<String>,
}

const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// Distinguishes "the token was rejected" from every other connection
/// failure. Previously both looked identical -- run_pull_loop retried
/// every RECONNECT_DELAY with the exact same doomed token forever, with no
/// way for anything (logs, the frontend) to tell "server unreachable" apart
/// from "please log in again."
enum ConnectError {
    Unauthorized,
    Other(String),
}

impl From<String> for ConnectError {
    fn from(s: String) -> Self {
        ConnectError::Other(s)
    }
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Unauthorized => write!(f, "unauthorized (401/403) -- token expired or invalid"),
            ConnectError::Other(s) => write!(f, "{s}"),
        }
    }
}

pub async fn run_pull_loop(ctx: Arc<SyncContext>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::SeqCst) {
        match connect_and_listen(&ctx, &stop).await {
            Ok(()) => {}
            Err(ConnectError::Unauthorized) => {
                // ctx.needs_reauth is already set by connect_and_listen
                // itself, right where the rejection is detected -- this arm
                // only logs it.
                eprintln!(
                    "prisma-desktop sync: WS connection to {} rejected as unauthorized -- \
                     waiting for re-login",
                    ctx.server_url
                );
            }
            Err(err @ ConnectError::Other(_)) => {
                // Logged (not just silently retried) so a genuinely broken
                // connection -- as opposed to ordinary offline/unreachable --
                // is at least diagnosable from stdout/journal. Confirmed live
                // 2026-07-26: a missing TLS backend produced the exact same
                // silent-forever-retry symptom as being offline, and was only
                // found by adding this logging back temporarily.
                eprintln!("prisma-desktop sync: WS connection to {} failed: {err}", ctx.server_url);
            }
        }
        *ctx.ws_outbound.lock().unwrap() = None;
        if stop.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

async fn connect_and_listen(ctx: &Arc<SyncContext>, stop: &Arc<AtomicBool>) -> Result<(), ConnectError> {
    let ws_url = to_ws_url(&ctx.server_url, &ctx.client_id);
    let mut request = ws_url.clone().into_client_request().map_err(|e| e.to_string())?;

    let token = ctx.token.lock().unwrap().clone();
    if let Some(token) = token {
        // Two offered subprotocol values: the fixed "bearer" marker (which
        // the server echoes back to confirm) and the token itself — see
        // AuthMiddleware's _bearer_from_ws for the matching server side.
        let value = format!("bearer, {token}");
        request.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            value.parse().map_err(|_| "invalid token for header".to_string())?,
        );
    }

    let (ws_stream, _response) = tokio_tungstenite::connect_async(request).await.map_err(|e| {
        // The server rejects the WS upgrade with a plain HTTP 401/403
        // (AuthMiddleware, matching every other authenticated endpoint) --
        // Error::Http carries that response when the handshake gets a
        // non-101 status back, distinct from every other failure mode
        // (DNS, refused connection, TLS, a dropped stream mid-handshake).
        if let tokio_tungstenite::tungstenite::Error::Http(resp) = &e {
            let status = resp.status().as_u16();
            if status == 401 || status == 403 {
                // Set here, at the point of detection, so this function is
                // fully self-contained regarding the flag's true/false
                // transitions -- run_pull_loop only logs it.
                ctx.needs_reauth.store(true, Ordering::SeqCst);
                return ConnectError::Unauthorized;
            }
        }
        ConnectError::Other(e.to_string())
    })?;
    // A live connection proves the token just worked -- clear any stale
    // "needs re-login" state from a previous rejected attempt.
    ctx.needs_reauth.store(false, Ordering::SeqCst);
    let (mut write, mut read) = ws_stream.split();

    // Give the fs watcher (and anything else) a way to send messages over
    // this connection without owning the socket itself.
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    *ctx.ws_outbound.lock().unwrap() = Some(tx);

    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        tokio::select! {
            incoming = read.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => handle_message(ctx, &text).await,
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Ok(_)) => {} // ping/pong/binary — nothing to do
                    Some(Err(e)) => return Err(ConnectError::Other(e.to_string())),
                }
            }
            outgoing = rx.recv() => {
                match outgoing {
                    Some(text) => {
                        if write.send(Message::Text(text.into())).await.is_err() {
                            return Ok(()); // connection dropped — outer loop reconnects
                        }
                    }
                    None => {} // channel sender dropped — connection is being torn down elsewhere
                }
            }
        }
    }
}

fn to_ws_url(server_url: &str, client_id: &str) -> String {
    let ws_base = if let Some(rest) = server_url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = server_url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        format!("ws://{server_url}")
    };
    format!("{}/ws?client_id={}", ws_base.trim_end_matches('/'), client_id)
}

async fn handle_message(ctx: &Arc<SyncContext>, text: &str) {
    let Ok(msg) = serde_json::from_str::<IncomingMessage>(text) else { return };
    match msg.kind.as_str() {
        "request_manifest" => {
            let files = super::manifest::build_manifest(&ctx.vault_path);
            super::send_ws_message(ctx, &serde_json::json!({ "type": "manifest_response", "files": files }));
        }
        "request_file" => {
            if let Some(path) = msg.path {
                // Server decided it needs this file — the same push logic
                // the (now-removed) watcher-triggered path used, complete
                // with its existing conflict/expected_mtime handling. The
                // HTTP response this produces is itself the ack the server
                // needs; no separate message required.
                super::push::push_path(ctx, &path).await;
            }
        }
        // Existing ADR-010 event type: both ordinary UI-originated vault
        // edits (from any connected client, not just this one) and this
        // client's own orchestrated push-down decisions arrive this way.
        "vault_change" => {
            let Some(path) = msg.path else { return };
            match msg.action.as_deref() {
                Some("sync_delete") | Some("delete") => {
                    super::mark_suppressed_write(ctx, &path);
                    let _ = std::fs::remove_file(ctx.vault_path.join(&path));
                    let mut state = ctx.state.lock().unwrap();
                    state.files.remove(&path);
                    super::save_sync_state(&state);
                }
                Some("sync_write") | Some("save") | Some("create") => {
                    super::mark_suppressed_write(ctx, &path);
                    let synced = pull_and_write(ctx, &path).await;
                    // ACK: the server doesn't consider a push-down complete
                    // until it hears back that this client actually applied
                    // it (mirrors a network ACK — see this module's own
                    // doc comment for why that matters here).
                    if let Some((hash, mtime)) = synced {
                        super::send_ws_message(
                            ctx,
                            &serde_json::json!({ "type": "file_synced", "path": path, "hash": hash, "mtime": mtime }),
                        );
                    }
                }
                _ => {}
            }
        }
        "ack" => {} // server acknowledging a file_changed/file_deleted notification — nothing to do
        _ => {}
    }
}

/// Pulls and writes `rel` locally. Returns `Some((hash, mtime))` on success
/// so the caller can ack the write back to the server (see the
/// "vault_change"/"sync_write" handler above) -- the server doesn't
/// consider a push-down complete until it hears this back. Was previously
/// in manifest.rs, a module documented as "pure diffing, no I/O" -- this
/// does real network + fs writes + state mutation, and is only ever called
/// from this module, so it belongs here.
async fn pull_and_write(ctx: &Arc<SyncContext>, rel: &str) -> Option<(String, f64)> {
    match super::pull_file(ctx, rel).await {
        Ok(Some((body, mtime))) => {
            let abs = ctx.vault_path.join(rel);
            if let Some(parent) = abs.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let hash = super::content_hash(&body);
            if std::fs::write(&abs, &body).is_ok() {
                let mut state = ctx.state.lock().unwrap();
                state.files.insert(rel.to_string(), TrackedFile { last_synced_mtime: mtime, content_hash: hash.clone() });
                super::save_sync_state(&state);
                return Some((hash, mtime));
            }
            None
        }
        Ok(None) => {
            // Deleted server-side between the manifest fetch and this
            // pull — nothing to write.
            None
        }
        Err(_) => {
            // Best-effort — a later WS event or reconciliation pass retries.
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_ws_url_converts_https_to_wss() {
        assert_eq!(
            to_ws_url("https://prisma.forge.internal", "client-1"),
            "wss://prisma.forge.internal/ws?client_id=client-1"
        );
    }

    #[test]
    fn to_ws_url_converts_http_to_ws() {
        assert_eq!(
            to_ws_url("http://127.0.0.1:8765", "client-1"),
            "ws://127.0.0.1:8765/ws?client_id=client-1"
        );
    }

    #[test]
    fn to_ws_url_falls_back_to_ws_when_no_scheme_prefix() {
        // Not expected in practice (server_url is always http(s):// per
        // settings.rs), but to_ws_url must not panic on it.
        assert_eq!(
            to_ws_url("127.0.0.1:8765", "client-1"),
            "ws://127.0.0.1:8765/ws?client_id=client-1"
        );
    }

    #[test]
    fn to_ws_url_strips_trailing_slash_before_appending_path() {
        assert_eq!(
            to_ws_url("http://127.0.0.1:8765/", "client-1"),
            "ws://127.0.0.1:8765/ws?client_id=client-1"
        );
    }

    // ── handle_message dispatch ──────────────────────────────────────────
    // Covers the branches that don't need a live HTTP server. request_file
    // calls through to push_path (its own test coverage lives in push.rs);
    // pull_and_write is exercised directly below, with a mock HTTP server.

    fn ctx_with_channel(vault_path: std::path::PathBuf) -> (Arc<SyncContext>, mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let ctx = Arc::new(SyncContext {
            vault_path,
            ws_outbound: std::sync::Mutex::new(Some(tx)),
            ..crate::sync::tests::test_ctx()
        });
        (ctx, rx)
    }

    #[test]
    fn handle_message_request_manifest_sends_manifest_response() {
        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("notes")).unwrap();
        std::fs::write(dir.join("notes/a.md"), "hello").unwrap();
        let (ctx, mut rx) = ctx_with_channel(dir.clone());

        tauri::async_runtime::block_on(handle_message(&ctx, r#"{"type":"request_manifest"}"#));

        let sent = rx.try_recv().expect("expected a manifest_response to be sent");
        let parsed: serde_json::Value = serde_json::from_str(&sent).unwrap();
        assert_eq!(parsed["type"], "manifest_response");
        assert_eq!(parsed["files"][0]["path"], "notes/a.md");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn handle_message_vault_change_sync_delete_removes_local_file_and_state() {
        let _guard = crate::json_store::TEST_ENV_GUARD.lock().unwrap();
        let config_dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}-cfg", uuid::Uuid::new_v4()));
        std::env::set_var("PRISMA_DESKTOP_CONFIG_DIR", &config_dir);

        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("notes")).unwrap();
        let file_path = dir.join("notes/a.md");
        std::fs::write(&file_path, "will be deleted").unwrap();

        let (ctx, _rx) = ctx_with_channel(dir.clone());
        ctx.state.lock().unwrap().files.insert(
            "notes/a.md".to_string(),
            crate::sync::TrackedFile { last_synced_mtime: 100.0, content_hash: "irrelevant".into() },
        );

        tauri::async_runtime::block_on(handle_message(
            &ctx,
            r#"{"type":"vault_change","action":"sync_delete","path":"notes/a.md"}"#,
        ));

        std::env::remove_var("PRISMA_DESKTOP_CONFIG_DIR");

        assert!(!file_path.exists());
        assert!(!ctx.state.lock().unwrap().files.contains_key("notes/a.md"));
        // the fs watcher's next event for this path must not echo back as a push
        assert!(super::super::consume_suppressed_write(&ctx, "notes/a.md"));

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&config_dir).ok();
    }

    #[test]
    fn handle_message_ack_is_a_noop() {
        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (ctx, mut rx) = ctx_with_channel(dir.clone());

        tauri::async_runtime::block_on(handle_message(&ctx, r#"{"type":"ack"}"#));

        assert!(rx.try_recv().is_err(), "ack must not trigger any outbound message");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn handle_message_unknown_type_is_ignored() {
        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (ctx, mut rx) = ctx_with_channel(dir.clone());

        tauri::async_runtime::block_on(handle_message(&ctx, r#"{"type":"something_new"}"#));

        assert!(rx.try_recv().is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn handle_message_malformed_json_does_not_panic() {
        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (ctx, mut rx) = ctx_with_channel(dir.clone());

        tauri::async_runtime::block_on(handle_message(&ctx, "not json at all"));

        assert!(rx.try_recv().is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    // ── pull_and_write ────────────────────────────────────────────────────
    // Previously in manifest.rs with zero coverage -- this module's own
    // spawn_mock_response (crate::sync::tests) makes it exercisable without
    // adding a mocking crate as a new dependency, same as push_file/pull_file's
    // tests in mod.rs.

    #[test]
    fn pull_and_write_writes_file_and_tracks_state_on_success() {
        let server_url = crate::sync::tests::spawn_mock_response(200, r#"{"body":"pulled content","mtime":200.0}"#);
        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("notes")).unwrap();
        let _guard = crate::json_store::TEST_ENV_GUARD.lock().unwrap();
        let config_dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}-cfg", uuid::Uuid::new_v4()));
        std::env::set_var("PRISMA_DESKTOP_CONFIG_DIR", &config_dir);

        let ctx = Arc::new(SyncContext { server_url, vault_path: dir.clone(), ..crate::sync::tests::test_ctx() });
        let result = tauri::async_runtime::block_on(pull_and_write(&ctx, "notes/a.md"));

        std::env::remove_var("PRISMA_DESKTOP_CONFIG_DIR");

        assert_eq!(result, Some((super::super::content_hash("pulled content"), 200.0)));
        assert_eq!(std::fs::read_to_string(dir.join("notes/a.md")).unwrap(), "pulled content");
        let state = ctx.state.lock().unwrap();
        let tracked = state.files.get("notes/a.md").expect("file should be tracked after pull");
        assert_eq!(tracked.last_synced_mtime, 200.0);
        assert_eq!(tracked.content_hash, super::super::content_hash("pulled content"));

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&config_dir).ok();
    }

    #[test]
    fn pull_and_write_creates_parent_dirs_for_nested_path() {
        let server_url = crate::sync::tests::spawn_mock_response(200, r#"{"body":"nested","mtime":100.0}"#);
        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let _guard = crate::json_store::TEST_ENV_GUARD.lock().unwrap();
        let config_dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}-cfg", uuid::Uuid::new_v4()));
        std::env::set_var("PRISMA_DESKTOP_CONFIG_DIR", &config_dir);

        let ctx = Arc::new(SyncContext { server_url, vault_path: dir.clone(), ..crate::sync::tests::test_ctx() });
        let result = tauri::async_runtime::block_on(pull_and_write(&ctx, "deep/nested/notes/a.md"));

        std::env::remove_var("PRISMA_DESKTOP_CONFIG_DIR");

        assert!(result.is_some());
        assert!(dir.join("deep/nested/notes/a.md").exists());

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&config_dir).ok();
    }

    #[test]
    fn pull_and_write_returns_none_when_deleted_server_side() {
        // 404: deleted on the server between the manifest fetch and this pull.
        let server_url = crate::sync::tests::spawn_mock_response(404, "");
        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let ctx = Arc::new(SyncContext { server_url, vault_path: dir.clone(), ..crate::sync::tests::test_ctx() });
        let result = tauri::async_runtime::block_on(pull_and_write(&ctx, "notes/gone.md"));

        assert_eq!(result, None);
        assert!(!dir.join("notes/gone.md").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pull_and_write_returns_none_on_server_error() {
        let server_url = crate::sync::tests::spawn_mock_response(500, "");
        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let ctx = Arc::new(SyncContext { server_url, vault_path: dir.clone(), ..crate::sync::tests::test_ctx() });
        let result = tauri::async_runtime::block_on(pull_and_write(&ctx, "notes/a.md"));

        assert_eq!(result, None);

        std::fs::remove_dir_all(&dir).ok();
    }

    // ── connect_and_listen: 401/403 detection ────────────────────────────
    // spawn_mock_response speaks plain HTTP, not a real WS upgrade -- but
    // that's exactly what's needed here: tokio-tungstenite treats any
    // non-101 response to the handshake request as Error::Http, regardless
    // of whether the server understands WebSocket at all, so a bare "here's
    // a 401" mock is a faithful stand-in for the real AuthMiddleware
    // rejection this is meant to catch.

    #[test]
    fn connect_and_listen_sets_needs_reauth_on_401() {
        let server_url = crate::sync::tests::spawn_mock_response(401, "");
        let ctx = Arc::new(SyncContext { server_url, ..crate::sync::tests::test_ctx() });
        let stop = Arc::new(AtomicBool::new(false));

        let result = tauri::async_runtime::block_on(connect_and_listen(&ctx, &stop));

        assert!(matches!(result, Err(ConnectError::Unauthorized)));
        assert!(ctx.needs_reauth.load(Ordering::SeqCst));
    }

    #[test]
    fn connect_and_listen_sets_needs_reauth_on_403() {
        let server_url = crate::sync::tests::spawn_mock_response(403, "");
        let ctx = Arc::new(SyncContext { server_url, ..crate::sync::tests::test_ctx() });
        let stop = Arc::new(AtomicBool::new(false));

        let result = tauri::async_runtime::block_on(connect_and_listen(&ctx, &stop));

        assert!(matches!(result, Err(ConnectError::Unauthorized)));
        assert!(ctx.needs_reauth.load(Ordering::SeqCst));
    }

    #[test]
    fn connect_and_listen_does_not_set_needs_reauth_on_other_errors() {
        // A 500 (or any other non-2xx/101 status, or a connection that
        // never even reaches an HTTP response) is a generic failure, not an
        // auth rejection -- must not be misdiagnosed as "please log in
        // again" when the real problem is e.g. the server being down.
        let server_url = crate::sync::tests::spawn_mock_response(500, "");
        let ctx = Arc::new(SyncContext { server_url, ..crate::sync::tests::test_ctx() });
        let stop = Arc::new(AtomicBool::new(false));

        let result = tauri::async_runtime::block_on(connect_and_listen(&ctx, &stop));

        assert!(matches!(result, Err(ConnectError::Other(_))));
        assert!(!ctx.needs_reauth.load(Ordering::SeqCst));
    }
}
