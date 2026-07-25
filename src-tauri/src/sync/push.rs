//! Local -> server: a debounced filesystem watcher on the vault_path
//! pushes whole-file creates/edits/deletes via PUT/DELETE /sync/file.

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
                    push_path(&ctx, &rel).await;
                }
            });
        },
    )?;
    debouncer.watch(&vault_path, RecursiveMode::Recursive)?;
    Ok(debouncer)
}

/// Pushes the current on-disk state of `rel` (or a delete, if it's gone) —
/// see the vault-sync plan's tracked/untracked lifecycle table. Also used
/// directly by initial reconciliation for PushNew/PushReCreate actions.
pub async fn push_path(ctx: &Arc<SyncContext>, rel: &str) {
    // A pending pull just wrote this exact path — this fs event is our own
    // write landing, not a real local edit. Consume it silently instead of
    // pushing it straight back (echo-loop suppression, desktop side).
    if super::consume_suppressed_write(ctx, rel) {
        return;
    }

    let abs = ctx.vault_path.join(rel);
    if abs.is_file() {
        let Ok(body) = std::fs::read_to_string(&abs) else { return };
        let expected_mtime = ctx.state.lock().unwrap().files.get(rel).map(|t| t.last_synced_mtime);
        match super::push_file(ctx, rel, &body, expected_mtime).await {
            Ok(new_mtime) => {
                let mut state = ctx.state.lock().unwrap();
                state.files.insert(rel.to_string(), TrackedFile { last_synced_mtime: new_mtime });
                super::save_sync_state(&state);
            }
            Err(PushError::Conflict { body: server_body, mtime: server_mtime }) => {
                super::resolve_conflict_push_side(ctx, rel, &body, server_body, server_mtime).await;
            }
            Err(PushError::Other(reason)) => {
                // Best-effort: the next debounced fs event or the next
                // reconciliation pass retries — no retry queue needed for
                // a personal, single-user tool. Logged so a stuck sync is
                // at least diagnosable from stdout/journal.
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
