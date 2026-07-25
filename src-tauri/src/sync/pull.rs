//! Server -> desktop: a persistent WebSocket connection receives
//! `vault_change` push notifications and pulls the changed file. Auth is
//! carried via the `Sec-WebSocket-Protocol` subprotocol (["bearer",
//! "<jwt>"]) rather than a query param, matching the server's
//! AuthMiddleware/_bearer_from_ws (see prisma/server/auth.py) — this keeps
//! the token out of URLs/logs on both sides of the connection.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde::Deserialize;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use super::SyncContext;

#[derive(Deserialize)]
struct VaultChangeEvent {
    #[serde(rename = "type")]
    kind: String,
    action: Option<String>,
    path: Option<String>,
}

const RECONNECT_DELAY: Duration = Duration::from_secs(5);

pub async fn run_pull_loop(ctx: Arc<SyncContext>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::SeqCst) {
        if let Err(_err) = connect_and_listen(&ctx, &stop).await {
            // Best-effort reconnect — offline/unreachable is a normal,
            // expected state for a personal LAN tool, not an error to
            // surface loudly.
        }
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
    let (_write, mut read) = ws_stream.split();

    while !stop.load(Ordering::SeqCst) {
        match read.next().await {
            Some(Ok(Message::Text(text))) => handle_message(ctx, &text).await,
            Some(Ok(Message::Close(_))) | None => break,
            Some(Ok(_)) => {} // ping/pong/binary — nothing to do
            Some(Err(e)) => return Err(e.to_string()),
        }
    }
    Ok(())
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
    let Ok(event) = serde_json::from_str::<VaultChangeEvent>(text) else { return };
    if event.kind != "vault_change" {
        return; // stream_progress and any future event types aren't sync's concern
    }
    let Some(path) = event.path else { return };
    match event.action.as_deref() {
        Some("sync_delete") => {
            let _ = std::fs::remove_file(ctx.vault_path.join(&path));
            let mut state = ctx.state.lock().unwrap();
            state.files.remove(&path);
            super::save_sync_state(&state);
        }
        Some("sync_write") | Some("save") | Some("create") => {
            // The write we're about to make will itself trigger the local
            // fs watcher — mark it so push.rs's handler recognizes and
            // skips it instead of pushing it straight back.
            super::mark_suppressed_write(ctx, &path);
            super::manifest::pull_and_write(ctx, &path).await;
        }
        Some("delete") => {
            super::mark_suppressed_write(ctx, &path);
            let _ = std::fs::remove_file(ctx.vault_path.join(&path));
            let mut state = ctx.state.lock().unwrap();
            state.files.remove(&path);
            super::save_sync_state(&state);
        }
        _ => {}
    }
}
