//! Local-vs-remote diffing. `reconcile` is pure, allocation-only logic used
//! today by sync_diff's UI preview ("what would sync do right now"); the
//! actual sync mechanism no longer applies its own diff at startup (see
//! pull.rs's module doc comment) -- the server orchestrates that now via
//! request_manifest. `build_manifest` is what actually feeds that: a full
//! local walk with a content hash per file, sent to the server on request.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;

use super::{relative_md_path, SyncContext, TrackedFile};

#[derive(Clone, Debug, PartialEq)]
pub struct LocalEntry {
    pub path: String,
    pub mtime: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RemoteEntry {
    pub path: String,
    pub mtime: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ReconcileAction {
    /// New locally, unknown to the server and never synced — push (create).
    PushNew(String),
    /// New on the server, not present locally and never synced — pull (create).
    PullNew(String),
    /// Was synced; now missing locally, remote unchanged since — propagate delete.
    PushDelete(String),
    /// Was synced; now missing on the server, local unchanged since — propagate delete.
    PullDelete(String),
    /// Was synced; missing locally AND changed remotely — ambiguous
    /// (local delete vs. remote edit) — favor not losing data, pull instead.
    PullUpdate(String),
    /// Was synced; missing on the server AND changed locally — ambiguous
    /// (local edit vs. remote delete) — favor not losing data, re-push as new.
    PushReCreate(String),
}

/// Pure diff: no I/O. `tracked` is sync_state.json's per-path record of the
/// mtime at last successful sync.
pub fn reconcile(
    local: &[LocalEntry],
    remote: &[RemoteEntry],
    tracked: &HashMap<String, TrackedFile>,
) -> Vec<ReconcileAction> {
    let local_map: HashMap<&str, &LocalEntry> = local.iter().map(|e| (e.path.as_str(), e)).collect();
    let remote_map: HashMap<&str, &RemoteEntry> = remote.iter().map(|e| (e.path.as_str(), e)).collect();

    let mut all_paths: BTreeSet<&str> = BTreeSet::new();
    all_paths.extend(local_map.keys());
    all_paths.extend(remote_map.keys());
    all_paths.extend(tracked.keys().map(|s| s.as_str()));

    let mut actions = Vec::new();
    for path in all_paths {
        let local_e = local_map.get(path);
        let remote_e = remote_map.get(path);
        let track = tracked.get(path);

        match (local_e, remote_e) {
            (Some(_), Some(_)) => {
                // Both present. Presence/absence isn't the issue here — a
                // real content conflict is caught by the normal push
                // path's expected_mtime check (409), not by this diff.
            }
            (Some(l), None) => match track {
                // Present locally, missing on the server.
                None => actions.push(ReconcileAction::PushNew(path.to_string())),
                Some(t) if l.mtime == t.last_synced_mtime => {
                    // Unchanged locally since last sync -> the server-side
                    // deletion (e.g. via the UI) should be mirrored here.
                    actions.push(ReconcileAction::PullDelete(path.to_string()))
                }
                Some(_) => actions.push(ReconcileAction::PushReCreate(path.to_string())),
            },
            (None, Some(r)) => match track {
                // Present on the server, missing locally.
                None => actions.push(ReconcileAction::PullNew(path.to_string())),
                Some(t) if r.mtime == t.last_synced_mtime => {
                    // Unchanged remotely since last sync -> the local
                    // deletion should be propagated to the server.
                    actions.push(ReconcileAction::PushDelete(path.to_string()))
                }
                Some(_) => actions.push(ReconcileAction::PullUpdate(path.to_string())),
            },
            (None, None) => {}
        }
    }
    actions
}

pub(crate) fn walk_local_md(vault_path: &Path) -> Vec<LocalEntry> {
    let mut out = Vec::new();
    let mut stack = vec![vault_path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Some(rel) = relative_md_path(vault_path, &path) else { continue };
            let Ok(meta) = entry.metadata() else { continue };
            let Ok(modified) = meta.modified() else { continue };
            let mtime = modified
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            out.push(LocalEntry { path: rel, mtime });
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct ManifestFileEntry {
    pub path: String,
    pub hash: String,
    pub mtime: f64,
    pub size: u64,
}

/// Full local manifest (path/hash/mtime/size for every synced file) sent to
/// the server in response to a `request_manifest` message -- see pull.rs.
/// The server is the sole orchestrator now: it diffs this against its own
/// vault content and a per-client baseline, and decides push vs. pull vs.
/// conflict per path (2026-07-26 redesign -- the fs watcher no longer
/// decides on its own to push; see push.rs's watcher for why that was
/// unsafe).
pub fn build_manifest(vault_path: &Path) -> Vec<ManifestFileEntry> {
    let mut out = Vec::new();
    let mut stack = vec![vault_path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Some(rel) = relative_md_path(vault_path, &path) else { continue };
            let Ok(meta) = entry.metadata() else { continue };
            let Ok(modified) = meta.modified() else { continue };
            let mtime = modified
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            let Ok(body) = std::fs::read_to_string(&path) else { continue };
            out.push(ManifestFileEntry {
                hash: super::content_hash(&body),
                mtime,
                size: meta.len(),
                path: rel,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracked(pairs: &[(&str, f64)]) -> HashMap<String, TrackedFile> {
        pairs
            .iter()
            .map(|(p, m)| (p.to_string(), TrackedFile { last_synced_mtime: *m, content_hash: String::new() }))
            .collect()
    }

    #[test]
    fn build_manifest_includes_path_hash_mtime_size() {
        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("notes")).unwrap();
        std::fs::write(dir.join("notes/a.md"), "hello").unwrap();

        let manifest = build_manifest(&dir);
        assert_eq!(manifest.len(), 1);
        assert_eq!(manifest[0].path, "notes/a.md");
        assert_eq!(manifest[0].hash, super::super::content_hash("hello"));
        assert_eq!(manifest[0].size, 5);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn build_manifest_skips_out_of_scope_files() {
        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/config"), "irrelevant").unwrap();
        std::fs::write(dir.join("readme.txt"), "not md or stream yaml").unwrap();

        let manifest = build_manifest(&dir);
        assert!(manifest.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn new_local_untracked_file_is_pushed() {
        let local = vec![LocalEntry { path: "notes/a.md".into(), mtime: 100.0 }];
        let actions = reconcile(&local, &[], &HashMap::new());
        assert_eq!(actions, vec![ReconcileAction::PushNew("notes/a.md".into())]);
    }

    #[test]
    fn new_remote_untracked_file_is_pulled() {
        let remote = vec![RemoteEntry { path: "notes/a.md".into(), mtime: 100.0 }];
        let actions = reconcile(&[], &remote, &HashMap::new());
        assert_eq!(actions, vec![ReconcileAction::PullNew("notes/a.md".into())]);
    }

    #[test]
    fn both_present_is_a_noop_at_the_manifest_level() {
        let local = vec![LocalEntry { path: "notes/a.md".into(), mtime: 100.0 }];
        let remote = vec![RemoteEntry { path: "notes/a.md".into(), mtime: 100.0 }];
        let actions = reconcile(&local, &remote, &HashMap::new());
        assert_eq!(actions, vec![]);
    }

    #[test]
    fn tracked_file_deleted_locally_unchanged_remotely_propagates_delete() {
        let remote = vec![RemoteEntry { path: "notes/a.md".into(), mtime: 100.0 }];
        let t = tracked(&[("notes/a.md", 100.0)]);
        let actions = reconcile(&[], &remote, &t);
        assert_eq!(actions, vec![ReconcileAction::PushDelete("notes/a.md".into())]);
    }

    #[test]
    fn tracked_file_deleted_locally_but_remote_changed_pulls_instead_of_deleting() {
        let remote = vec![RemoteEntry { path: "notes/a.md".into(), mtime: 200.0 }];
        let t = tracked(&[("notes/a.md", 100.0)]);
        let actions = reconcile(&[], &remote, &t);
        assert_eq!(actions, vec![ReconcileAction::PullUpdate("notes/a.md".into())]);
    }

    #[test]
    fn tracked_file_deleted_remotely_unchanged_locally_propagates_delete() {
        let local = vec![LocalEntry { path: "notes/a.md".into(), mtime: 100.0 }];
        let t = tracked(&[("notes/a.md", 100.0)]);
        let actions = reconcile(&local, &[], &t);
        assert_eq!(actions, vec![ReconcileAction::PullDelete("notes/a.md".into())]);
    }

    #[test]
    fn tracked_file_deleted_remotely_but_local_changed_repushes_instead_of_deleting() {
        let local = vec![LocalEntry { path: "notes/a.md".into(), mtime: 200.0 }];
        let t = tracked(&[("notes/a.md", 100.0)]);
        let actions = reconcile(&local, &[], &t);
        assert_eq!(actions, vec![ReconcileAction::PushReCreate("notes/a.md".into())]);
    }

    #[test]
    fn absent_everywhere_is_a_noop() {
        let t = tracked(&[("notes/ghost.md", 100.0)]);
        let actions = reconcile(&[], &[], &t);
        assert_eq!(actions, vec![]);
    }

    #[test]
    fn multiple_paths_are_each_diffed_independently() {
        let local = vec![
            LocalEntry { path: "notes/new-local.md".into(), mtime: 100.0 },
            LocalEntry { path: "notes/both.md".into(), mtime: 100.0 },
        ];
        let remote = vec![
            RemoteEntry { path: "notes/new-remote.md".into(), mtime: 100.0 },
            RemoteEntry { path: "notes/both.md".into(), mtime: 100.0 },
        ];
        let mut actions = reconcile(&local, &remote, &HashMap::new());
        actions.sort_by_key(|a| format!("{a:?}"));
        assert_eq!(
            actions,
            vec![
                ReconcileAction::PullNew("notes/new-remote.md".into()),
                ReconcileAction::PushNew("notes/new-local.md".into()),
            ]
        );
    }
}

/// Pulls and writes `rel` locally. Returns `Some((hash, mtime))` on success
/// so the caller can ack the write back to the server (see pull.rs's
/// "vault_change"/"sync_write" handler) -- the server doesn't consider a
/// push-down complete until it hears this back.
pub async fn pull_and_write(ctx: &Arc<SyncContext>, rel: &str) -> Option<(String, f64)> {
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
