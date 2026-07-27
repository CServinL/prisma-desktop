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

use super::SyncContext;

#[derive(Deserialize)]
struct IncomingMessage {
    #[serde(rename = "type")]
    kind: String,
    action: Option<String>,
    path: Option<String>,
}

const RECONNECT_DELAY: Duration = Duration::from_secs(5);

pub async fn run_pull_loop(ctx: Arc<SyncContext>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::SeqCst) {
        if let Err(err) = connect_and_listen(&ctx, &stop).await {
            // Logged (not just silently retried) so a genuinely broken
            // connection -- as opposed to ordinary offline/unreachable --
            // is at least diagnosable from stdout/journal. Confirmed live
            // 2026-07-26: a missing TLS backend produced the exact same
            // silent-forever-retry symptom as being offline, and was only
            // found by adding this logging back temporarily.
            eprintln!("prisma-desktop sync: WS connection to {} failed: {err}", ctx.server_url);
        }
        *ctx.ws_outbound.lock().unwrap() = None;
        if stop.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

async fn connect_and_listen(ctx: &Arc<SyncContext>, stop: &Arc<AtomicBool>) -> Result<(), String> {
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

    let (ws_stream, _response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| e.to_string())?;
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
                    Some(Err(e)) => return Err(e.to_string()),
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
                    let synced = super::manifest::pull_and_write(ctx, &path).await;
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
    // Covers the branches that don't need a live HTTP server
    // (request_file and vault_change/sync_write both call through to
    // push_path/pull_and_write, which make real HTTP calls -- left for a
    // follow-up with an HTTP mock).

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
}
