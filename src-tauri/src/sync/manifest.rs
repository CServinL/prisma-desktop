//! Initial-reconciliation diffing: local walk vs. server manifest vs.
//! sync_state.json's "tracked" record — see the vault-sync plan's lifecycle
//! table. Kept as pure, allocation-only logic (`reconcile`) so it's
//! unit-testable with no filesystem or network; `run_initial_reconciliation`
//! is the thin, untested I/O shell around it.

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

/// Runs once at sync startup, before the fs-watcher/WS loop take over live
/// changes.
pub async fn run_initial_reconciliation(ctx: &Arc<SyncContext>) {
    let local = walk_local_md(&ctx.vault_path);

    let remote: Vec<RemoteEntry> = match super::fetch_manifest(ctx).await {
        Ok(entries) => entries,
        Err(_) => return, // offline/unreachable — live sync will catch up once reachable
    };

    let tracked = ctx.state.lock().unwrap().files.clone();
    let actions = reconcile(&local, &remote, &tracked);

    for action in actions {
        match action {
            ReconcileAction::PushNew(path) | ReconcileAction::PushReCreate(path) => {
                super::push::push_path(ctx, &path).await;
            }
            ReconcileAction::PullNew(path) | ReconcileAction::PullUpdate(path) => {
                pull_and_write(ctx, &path).await;
            }
            ReconcileAction::PushDelete(path) => {
                let _ = super::delete_remote(ctx, &path).await;
                let mut state = ctx.state.lock().unwrap();
                state.files.remove(&path);
                super::save_sync_state(&state);
            }
            ReconcileAction::PullDelete(path) => {
                let _ = std::fs::remove_file(ctx.vault_path.join(&path));
                let mut state = ctx.state.lock().unwrap();
                state.files.remove(&path);
                super::save_sync_state(&state);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracked(pairs: &[(&str, f64)]) -> HashMap<String, TrackedFile> {
        pairs
            .iter()
            .map(|(p, m)| (p.to_string(), TrackedFile { last_synced_mtime: *m }))
            .collect()
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

pub async fn pull_and_write(ctx: &Arc<SyncContext>, rel: &str) {
    match super::pull_file(ctx, rel).await {
        Ok(Some((body, mtime))) => {
            let abs = ctx.vault_path.join(rel);
            if let Some(parent) = abs.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if std::fs::write(&abs, &body).is_ok() {
                let mut state = ctx.state.lock().unwrap();
                state.files.insert(rel.to_string(), TrackedFile { last_synced_mtime: mtime });
                super::save_sync_state(&state);
            }
        }
        Ok(None) => {
            // Deleted server-side between the manifest fetch and this
            // pull — nothing to write.
        }
        Err(_) => {
            // Best-effort — a later WS event or reconciliation pass retries.
        }
    }
}
