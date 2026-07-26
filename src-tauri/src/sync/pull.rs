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
