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

use super::manifest::ManifestFileEntry;
use super::{relative_md_path, PushError, SyncContext, TrackedFile};

/// Wraps a ManifestFileEntry with the "type" field the WS protocol needs --
/// #[serde(flatten)] merges path/hash/mtime/size alongside it, producing
/// the exact same {"type":"file_changed","path":...,"hash":...,"mtime":...,
/// "size":...} shape this used to build by hand with serde_json::json!(),
/// but now sharing ManifestFileEntry's one definition of "how a file is
/// described" with manifest.rs::build_manifest instead of a second,
/// independently-typed copy of the same four fields.
#[derive(serde::Serialize)]
struct FileChangedMessage<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(flatten)]
    entry: &'a ManifestFileEntry,
}

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
        let entry = ManifestFileEntry { path: rel.to_string(), hash, mtime, size: meta.len() };
        super::send_ws_message(ctx, &FileChangedMessage { kind: "file_changed", entry: &entry });
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // notify_change never calls save_sync_state or any HTTP endpoint (only
    // send_ws_message, an in-memory channel send) -- no config-dir override
    // or HTTP mock needed, unlike push_path below it.

    fn ctx_with_channel(vault_path: PathBuf) -> (Arc<SyncContext>, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = Arc::new(SyncContext {
            vault_path,
            ws_outbound: Mutex::new(Some(tx)),
            ..crate::sync::tests::test_ctx()
        });
        (ctx, rx)
    }

    #[test]
    fn notify_change_skips_notification_when_content_hash_unchanged() {
        // The actual fix for the real, previously-shipped bug this module's
        // doc comment describes: a watcher event alone is not proof content
        // changed, so an unchanged hash must not even notify the server.
        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("notes")).unwrap();
        std::fs::write(dir.join("notes/a.md"), "same content").unwrap();
        let (ctx, mut rx) = ctx_with_channel(dir.clone());
        let hash = super::super::content_hash("same content");
        ctx.state.lock().unwrap().files.insert(
            "notes/a.md".to_string(),
            TrackedFile { last_synced_mtime: 100.0, content_hash: hash },
        );

        tauri::async_runtime::block_on(notify_change(&ctx, "notes/a.md"));

        assert!(rx.try_recv().is_err(), "unchanged content must not notify the server");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn notify_change_notifies_when_content_hash_differs_from_tracked() {
        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("notes")).unwrap();
        std::fs::write(dir.join("notes/a.md"), "new content").unwrap();
        let (ctx, mut rx) = ctx_with_channel(dir.clone());
        ctx.state.lock().unwrap().files.insert(
            "notes/a.md".to_string(),
            TrackedFile { last_synced_mtime: 100.0, content_hash: "stale-hash".into() },
        );

        tauri::async_runtime::block_on(notify_change(&ctx, "notes/a.md"));

        let sent = rx.try_recv().expect("expected a file_changed notification");
        let parsed: serde_json::Value = serde_json::from_str(&sent).unwrap();
        assert_eq!(parsed["type"], "file_changed");
        assert_eq!(parsed["path"], "notes/a.md");
        assert_eq!(parsed["hash"], super::super::content_hash("new content"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn notify_change_notifies_for_new_untracked_file() {
        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("notes")).unwrap();
        std::fs::write(dir.join("notes/a.md"), "brand new").unwrap();
        let (ctx, mut rx) = ctx_with_channel(dir.clone());

        tauri::async_runtime::block_on(notify_change(&ctx, "notes/a.md"));

        let sent = rx.try_recv().expect("expected a file_changed notification");
        let parsed: serde_json::Value = serde_json::from_str(&sent).unwrap();
        assert_eq!(parsed["type"], "file_changed");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn notify_change_notifies_delete_for_previously_tracked_file() {
        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (ctx, mut rx) = ctx_with_channel(dir.clone());
        ctx.state.lock().unwrap().files.insert(
            "notes/gone.md".to_string(),
            TrackedFile { last_synced_mtime: 100.0, content_hash: "whatever".into() },
        );

        tauri::async_runtime::block_on(notify_change(&ctx, "notes/gone.md"));

        let sent = rx.try_recv().expect("expected a file_deleted notification");
        let parsed: serde_json::Value = serde_json::from_str(&sent).unwrap();
        assert_eq!(parsed["type"], "file_deleted");
        assert_eq!(parsed["path"], "notes/gone.md");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn notify_change_ignores_delete_for_never_tracked_file() {
        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (ctx, mut rx) = ctx_with_channel(dir.clone());

        tauri::async_runtime::block_on(notify_change(&ctx, "notes/never-existed.md"));

        assert!(rx.try_recv().is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn notify_change_skips_when_write_is_suppressed() {
        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("notes")).unwrap();
        std::fs::write(dir.join("notes/a.md"), "just pulled from server").unwrap();
        let (ctx, mut rx) = ctx_with_channel(dir.clone());
        super::super::mark_suppressed_write(&ctx, "notes/a.md");

        tauri::async_runtime::block_on(notify_change(&ctx, "notes/a.md"));

        assert!(rx.try_recv().is_err(), "our own pull-driven write must not echo back as a notification");
        std::fs::remove_dir_all(&dir).ok();
    }
}
