//! Local-vs-remote diffing. `reconcile` is pure, allocation-only logic used
//! today by sync_diff's UI preview ("what would sync do right now"); the
//! actual sync mechanism no longer applies its own diff at startup (see
//! pull.rs's module doc comment) -- the server orchestrates that now via
//! request_manifest. `build_manifest` is what actually feeds that: a full
//! local walk with a content hash per file, sent to the server on request.
//! Pure diffing/building only -- no network, no fs writes beyond the read
//! side of the walk (see pull.rs::pull_and_write for the network+write half
//! this module used to also hold).

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use super::{relative_md_path, TrackedFile};

/// Same tolerance as sync_routes.py's `_MTIME_TOLERANCE_SECONDS`. Exact `==`
/// on Unix-timestamp-magnitude f64s (~1.78e9 at this scale) can fail after a
/// cross-language JSON round-trip: the two ends land one float64 ULP apart
/// (~238ns at this magnitude) even though nothing actually changed. That bug
/// was already found and fixed on the Python side (prisma#40) — this mirrors
/// the same fix here rather than reintroducing exact equality.
const MTIME_TOLERANCE_SECONDS: f64 = 1e-3;

fn mtime_unchanged(a: f64, b: f64) -> bool {
    (a - b).abs() <= MTIME_TOLERANCE_SECONDS
}

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
                Some(t) if mtime_unchanged(l.mtime, t.last_synced_mtime) => {
                    // Unchanged locally since last sync -> the server-side
                    // deletion (e.g. via the UI) should be mirrored here.
                    actions.push(ReconcileAction::PullDelete(path.to_string()))
                }
                Some(_) => actions.push(ReconcileAction::PushReCreate(path.to_string())),
            },
            (None, Some(r)) => match track {
                // Present on the server, missing locally.
                None => actions.push(ReconcileAction::PullNew(path.to_string())),
                Some(t) if mtime_unchanged(r.mtime, t.last_synced_mtime) => {
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

/// Its own tree walk previously duplicated build_manifest's byte-for-byte
/// (same walk, same mtime computation, minus the hash) -- now expressed as
/// build_manifest's result mapped down to just path/mtime, so there is
/// exactly one traversal implementation instead of two that could silently
/// diverge (e.g. a future symlink-handling or skip-condition fix landing in
/// only one of them).
pub(crate) fn walk_local_md(vault_path: &Path) -> Vec<LocalEntry> {
    build_manifest(vault_path)
        .into_iter()
        .map(|e| LocalEntry { path: e.path, mtime: e.mtime })
        .collect()
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

    /// Single-path branches of reconcile()'s tracked/untracked lifecycle
    /// table -- was 8 separate ~4-line functions (the same shape as
    /// prisma's own diff_manifest test suite, its documented mirror);
    /// collapsed into one table-driven test for the same reason that one
    /// was parametrized. Each row still traces to a named case, same as
    /// before, just without the boilerplate per row.
    #[test]
    fn reconcile_single_path_decision_table() {
        struct Case {
            name: &'static str,
            local_mtime: Option<f64>,
            remote_mtime: Option<f64>,
            tracked_mtime: Option<f64>,
            expected: Option<ReconcileAction>,
        }
        let cases = [
            Case {
                name: "new_local_untracked_file_is_pushed",
                local_mtime: Some(100.0), remote_mtime: None, tracked_mtime: None,
                expected: Some(ReconcileAction::PushNew("notes/a.md".into())),
            },
            Case {
                name: "new_remote_untracked_file_is_pulled",
                local_mtime: None, remote_mtime: Some(100.0), tracked_mtime: None,
                expected: Some(ReconcileAction::PullNew("notes/a.md".into())),
            },
            Case {
                name: "both_present_is_a_noop_at_the_manifest_level",
                local_mtime: Some(100.0), remote_mtime: Some(100.0), tracked_mtime: None,
                expected: None,
            },
            Case {
                name: "tracked_file_deleted_locally_unchanged_remotely_propagates_delete",
                local_mtime: None, remote_mtime: Some(100.0), tracked_mtime: Some(100.0),
                expected: Some(ReconcileAction::PushDelete("notes/a.md".into())),
            },
            Case {
                name: "tracked_file_deleted_locally_but_remote_changed_pulls_instead_of_deleting",
                local_mtime: None, remote_mtime: Some(200.0), tracked_mtime: Some(100.0),
                expected: Some(ReconcileAction::PullUpdate("notes/a.md".into())),
            },
            Case {
                name: "tracked_file_deleted_remotely_unchanged_locally_propagates_delete",
                local_mtime: Some(100.0), remote_mtime: None, tracked_mtime: Some(100.0),
                expected: Some(ReconcileAction::PullDelete("notes/a.md".into())),
            },
            Case {
                name: "tracked_file_deleted_remotely_but_local_changed_repushes_instead_of_deleting",
                local_mtime: Some(200.0), remote_mtime: None, tracked_mtime: Some(100.0),
                expected: Some(ReconcileAction::PushReCreate("notes/a.md".into())),
            },
            Case {
                // local/remote both absent is a noop regardless of tracked
                // state (reconcile's (None, None) arm never looks at it).
                name: "absent_everywhere_is_a_noop",
                local_mtime: None, remote_mtime: None, tracked_mtime: None,
                expected: None,
            },
        ];

        for case in cases {
            let local: Vec<LocalEntry> = case.local_mtime
                .map(|m| vec![LocalEntry { path: "notes/a.md".into(), mtime: m }])
                .unwrap_or_default();
            let remote: Vec<RemoteEntry> = case.remote_mtime
                .map(|m| vec![RemoteEntry { path: "notes/a.md".into(), mtime: m }])
                .unwrap_or_default();
            let t = case.tracked_mtime
                .map(|m| tracked(&[("notes/a.md", m)]))
                .unwrap_or_default();

            let actions = reconcile(&local, &remote, &t);
            let expected: Vec<ReconcileAction> = case.expected.into_iter().collect();
            assert_eq!(actions, expected, "case: {}", case.name);
        }
    }

    #[test]
    fn mtime_one_ulp_apart_is_still_treated_as_unchanged() {
        // Regression test for the float-ULP bug already fixed on the Python
        // side (prisma#40): exact `==` on a Unix-timestamp-magnitude f64
        // fails after a cross-language JSON round-trip even with no real
        // change, because the two ends can land one float64 ULP apart.
        let synced_mtime = 1_700_000_000.123_456_7_f64;
        let one_ulp_later = synced_mtime.next_up();
        assert_ne!(synced_mtime, one_ulp_later, "test setup must pick two distinct floats");

        let local = vec![LocalEntry { path: "notes/a.md".into(), mtime: one_ulp_later }];
        let t = tracked(&[("notes/a.md", synced_mtime)]);
        // Present locally, absent on server, tracked — should read as
        // "unchanged since last sync" (PullDelete), not PushReCreate.
        let actions = reconcile(&local, &[], &t);
        assert_eq!(actions, vec![ReconcileAction::PullDelete("notes/a.md".into())]);
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
