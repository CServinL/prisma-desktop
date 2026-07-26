//! Local fs-watcher: notifies the server of local changes (stat + content
//! hash only, never content itself) and pushes a file's actual content only
//! when the server explicitly asks for it via a `request_file` WS message
//! (see pull.rs). This watcher used to decide on its own to push whenever
//! it fired — that caused a real, sustained bug (2026-07-26): the watcher
//! firing is not proof content changed, and with the watcher as an
//! independent decision-maker there was nothing to stop a spurious/
//! duplicate event from looping. The server is now the sole orchestrator;
//! this file's only job is to tell it "something happened here," never to
//! act unilaterally.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, RecommendedCache};

use super::{relative_md_path, PushError, SyncContext, TrackedFile};

pub type WatcherHandle = Debouncer<notify::RecommendedWatcher, RecommendedCache>;

pub fn start_watcher(ctx: Arc<SyncContext>) -> notify::Result<WatcherHandle> {
    let vault_path = ctx.vault_path.clone();
    let mut debouncer = new_debouncer(
        Duration::from_millis(800),
        None,
        move |result: DebounceEventResult| {
            let Ok(events) = result else { return };
            let ctx = ctx.clone();
            let paths: Vec<PathBuf> = events.iter().flat_map(|e| e.paths.clone()).collect();
            tauri::async_runtime::spawn(async move {
                for abs_path in paths {
                    let Some(rel) = relative_md_path(&ctx.vault_path, &abs_path) else { continue };
                    notify_change(&ctx, &rel).await;
                }
            });
        },
    )?;
    debouncer.watch(&vault_path, RecursiveMode::Recursive)?;
    Ok(debouncer)
}

/// Tells the server "this path may have changed" — stat + content hash
/// only, never the file's actual content. The server decides whether it
/// actually needs the file (via `request_file`) or already has it (a bare
/// `ack`). Also the one place that suppresses our own pull-driven writes,
/// same as push_path used to.
async fn notify_change(ctx: &Arc<SyncContext>, rel: &str) {
    if super::consume_suppressed_write(ctx, rel) {
        return;
    }

    let abs = ctx.vault_path.join(rel);
    if let Ok(body) = std::fs::read_to_string(&abs) {
        let hash = super::content_hash(&body);
        // Same false-positive guard push_path already had: the watcher
        // firing isn't proof anything really changed, so don't even bother
        // the server with a notification for content it already knows about.
        let already_known = ctx
            .state
            .lock()
            .unwrap()
            .files
            .get(rel)
            .map(|t| t.content_hash == hash)
            .unwrap_or(false);
        if already_known {
            return;
        }
        let Ok(meta) = std::fs::metadata(&abs) else { return };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        super::send_ws_message(
            ctx,
            &serde_json::json!({
                "type": "file_changed", "path": rel, "hash": hash, "mtime": mtime, "size": meta.len(),
            }),
        );
    } else {
        let was_tracked = ctx.state.lock().unwrap().files.contains_key(rel);
        if !was_tracked {
            return; // never synced — nothing to tell the server
        }
        super::send_ws_message(ctx, &serde_json::json!({ "type": "file_deleted", "path": rel }));
    }
}

/// Pushes the current on-disk state of `rel` (or a delete, if it's gone) to
/// the server. Called only in response to the server's own `request_file`
/// message (pull.rs) — never triggered directly by the fs watcher, see this
/// module's doc comment for why.
pub async fn push_path(ctx: &Arc<SyncContext>, rel: &str) {
    let abs = ctx.vault_path.join(rel);
    if abs.is_file() {
        let Ok(body) = std::fs::read_to_string(&abs) else { return };
        let hash = super::content_hash(&body);
        let expected_mtime = ctx.state.lock().unwrap().files.get(rel).map(|t| t.last_synced_mtime);
        match super::push_file(ctx, rel, &body, expected_mtime).await {
            Ok(new_mtime) => {
                let mut state = ctx.state.lock().unwrap();
                state.files.insert(rel.to_string(), TrackedFile { last_synced_mtime: new_mtime, content_hash: hash });
                super::save_sync_state(&state);
            }
            Err(PushError::Conflict { body: server_body, mtime: server_mtime }) => {
                super::resolve_conflict_push_side(ctx, rel, &body, server_body, server_mtime).await;
            }
            Err(PushError::Other(reason)) => {
                // Best-effort: the server can always request_file again
                // later — no retry queue needed for a personal, single-user
                // tool. Logged so a stuck sync is at least diagnosable.
                eprintln!("prisma-desktop sync: push {rel} failed: {reason}");
            }
        }
    } else {
        let was_tracked = ctx.state.lock().unwrap().files.contains_key(rel);
        if !was_tracked {
            return; // never synced — nothing to tell the server
        }
        let _ = super::delete_remote(ctx, rel).await;
        let mut state = ctx.state.lock().unwrap();
        state.files.remove(rel);
        super::save_sync_state(&state);
    }
}
