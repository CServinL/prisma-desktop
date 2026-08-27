//! Vault-sync engine: local `.md` copy <-> server, via /sync/* (whole-file,
//! path-based) and /ws (server-push notifications). KG/Chroma stay
//! server-side only — this is the entire desktop-side surface for that.
//! See the vault-sync plan for the full design (echo-loop prevention,
//! conflict handling, tracked/untracked lifecycle table).

pub mod manifest;
pub mod pull;
pub mod push;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::Manager;

pub use manifest::RemoteEntry;

// ── Persistent per-path sync tracking (sync_state.json) ──────────────────────

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct TrackedFile {
    pub last_synced_mtime: f64,
    /// SHA256 hex digest of the content as of the last successful sync
    /// (push or pull). The single source of truth for "did this file's
    /// content actually change" -- the fs watcher firing is NOT proof of
    /// that on its own (confirmed live 2026-07-26: a sustained loop of
    /// hundreds of pushes for unchanged content, with no corresponding real
    /// edits). KG/Chroma already learned this exact lesson (see
    /// knowledge_graph_service.py's _indexed_hash/_set_indexed_hash) but the
    /// sync engine never got the same treatment -- push_path pushed
    /// whatever the watcher told it to, unconditionally. Empty string means
    /// "never synced" (any real content differs from it, so the first push
    /// always proceeds).
    #[serde(default)]
    pub content_hash: String,
}

/// SHA256 hex digest of `body` -- see TrackedFile::content_hash.
pub fn content_hash(body: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// See settings.rs's SETTINGS_SCHEMA_VERSION for the convention this
/// mirrors. No real migration needed yet.
pub const SYNC_STATE_SCHEMA_VERSION: u32 = 1;

fn sync_state_migration_chain() -> crate::schema_gov::MigrationChain {
    crate::schema_gov::MigrationChain { current_version: SYNC_STATE_SCHEMA_VERSION, migrations: HashMap::new() }
}

fn default_schema_version() -> u32 { 1 }

#[derive(Serialize, Deserialize, Clone)]
pub struct SyncState {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub client_id: Option<String>,
    pub files: HashMap<String, TrackedFile>,
}

impl Default for SyncState {
    fn default() -> Self {
        Self { schema_version: SYNC_STATE_SCHEMA_VERSION, client_id: None, files: HashMap::new() }
    }
}

fn sync_state_path() -> PathBuf {
    crate::json_store::config_file_path("sync_state.json")
}

pub fn load_sync_state() -> SyncState {
    crate::json_store::load_json(&sync_state_path(), &sync_state_migration_chain())
}

pub fn save_sync_state(state: &SyncState) {
    crate::json_store::save_json(&sync_state_path(), state, false)
}

fn client_id() -> String {
    let mut state = load_sync_state();
    if let Some(id) = &state.client_id {
        return id.clone();
    }
    let id = uuid::Uuid::new_v4().to_string();
    state.client_id = Some(id.clone());
    save_sync_state(&state);
    id
}

// ── Path scoping (mirrors the server's VaultService._SKIP_DIRS) ──────────────
// Internal app state (chromadb/, kg-out/) lives under .vault-files/ rather
// than being listed here by name — the leading-dot rule below already
// excludes it, the same way .git is excluded, so a new internal dir never
// needs a new entry.

const SKIP_DIRS: &[&str] = &[
    ".git", ".svn", "__pycache__", "node_modules", ".venv", "venv", "dist", "build",
];

/// Absolute fs-watcher path -> vault-relative, forward-slash sync path, or
/// None if out of scope for sync: not a recognised content type, inside a
/// skip/hidden dir, or a conflict-copy file (see `write_conflict_copy`).
/// Excluding conflict copies here is what stops them from being treated as
/// new syncable content: without this, each one gets pushed, can conflict
/// again, and spawns another nested `.conflict-<ts>` copy — unbounded
/// recursive duplication (confirmed live 2026-07-25: 51 corrupted files from
/// a handful of app restarts before this exclusion existed).
///
/// `streams/*.yaml` is the one other real vault-content type besides `.md`
/// notes (server's VaultService.create_stream/save_stream) — special-cased
/// by directory, the opposite way from `.vault-files/` (internal app state,
/// excluded entirely via the leading-dot rule): this is a content dir with
/// its own known type, not something to hide from sync. Must mirror the
/// server's `_safe_sync_path` exactly, or the two sides silently disagree
/// about what's in scope.
pub fn relative_md_path(vault_path: &Path, abs_path: &Path) -> Option<String> {
    let file_name = abs_path.file_name().and_then(|n| n.to_str())?;
    if file_name.contains(".conflict-") {
        return None;
    }
    let rel = abs_path.strip_prefix(vault_path).ok()?;
    let parts: Vec<&str> = rel.iter().map(|p| p.to_str().unwrap_or("")).collect();
    if parts.iter().any(|p| SKIP_DIRS.contains(p) || p.starts_with('.')) {
        return None;
    }
    let ext = abs_path.extension().and_then(|e| e.to_str());
    let is_stream_yaml = parts.first() == Some(&"streams") && ext == Some("yaml");
    if ext != Some("md") && !is_stream_yaml {
        return None;
    }
    Some(parts.join("/"))
}

// ── Shared context + HTTP helpers ─────────────────────────────────────────────

/// The one place a `reqwest::Client` for talking to the prisma server gets
/// built -- `reqwest::Client::new()` has no request timeout at all by
/// default, so a request whose TCP connection is accepted but never
/// answered (a pod dying mid-request during a rolling deploy is the
/// concrete case that surfaced this, 2026-08-27) hangs the awaiting task
/// forever, not just until some retry logic gives up. `pull.rs`'s
/// run_pull_loop already retries a *failed* connection every
/// RECONNECT_DELAY -- this is what makes sure a request can actually fail
/// in the first place, instead of a whole sync-engine tokio task (and
/// every other task competing for the same worker threads) sitting stuck
/// at 0% CPU with nothing to show for it, not even the app's own UI
/// reload working. auth::sync_login had the same gap independently.
const HTTP_CLIENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn build_http_client() -> reqwest::Client {
    build_http_client_with_timeout(HTTP_CLIENT_TIMEOUT)
}

/// Split out from build_http_client() so a test can exercise the actual
/// timeout mechanism against a short duration instead of waiting out the
/// real 30s production value.
fn build_http_client_with_timeout(timeout: std::time::Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .expect("reqwest client should build (see pull.rs's own note on a missing TLS backend -- a real, previously-seen failure mode for this exact call, not hypothetical)")
}

pub struct SyncContext {
    pub http: reqwest::Client,
    pub server_url: String,
    pub vault_path: PathBuf,
    pub client_id: String,
    pub token: Mutex<Option<String>>,
    pub state: Mutex<SyncState>,
    /// Paths whose next local fs-watcher event should be silently consumed
    /// instead of pushed — set right before a pull-driven write lands, so
    /// that write doesn't echo straight back to the server as a push.
    suppress_writes: Mutex<HashSet<String>>,
    /// Set by pull::connect_and_listen once the WS connection is live;
    /// lets any other part of the engine (the fs watcher, in particular)
    /// send a message to the server over that same connection without
    /// owning the socket itself. None when disconnected -- sends are
    /// best-effort, matching this tool's existing "offline is normal"
    /// philosophy (see pull::run_pull_loop's reconnect loop).
    pub ws_outbound: Mutex<Option<tokio::sync::mpsc::UnboundedSender<String>>>,
    /// Set when the WS handshake is rejected with 401/403 -- the current
    /// token is invalid/expired and nothing in this engine can refresh it
    /// on its own: password-mode auth (ADR-011) has no refresh-token
    /// concept, and this process never stores the password, only the
    /// token. Previously this looked identical to "server unreachable" --
    /// run_pull_loop retried forever every RECONNECT_DELAY with the exact
    /// same doomed token. Cleared on the next successful connection, or
    /// immediately when sync_login (auth/mod.rs) hands this context a
    /// fresh token via update_running_token. Exposed via SyncStatusInfo so
    /// the frontend can tell the two states apart.
    pub needs_reauth: AtomicBool,
}

/// Best-effort send of `msg` to the server over the live WS connection, if
/// any. Silently drops the message when disconnected -- the next
/// request_manifest on reconnect (see pull.rs) is what actually guarantees
/// eventual consistency, not this notification.
pub fn send_ws_message(ctx: &SyncContext, msg: &impl Serialize) {
    if let Some(tx) = ctx.ws_outbound.lock().unwrap().as_ref() {
        if let Ok(text) = serde_json::to_string(msg) {
            let _ = tx.send(text);
        }
    }
}

pub fn mark_suppressed_write(ctx: &SyncContext, rel: &str) {
    ctx.suppress_writes.lock().unwrap().insert(rel.to_string());
}

/// Returns true (and consumes the marker) if `rel`'s upcoming fs event is
/// our own pull-driven write, not a real local edit.
pub fn consume_suppressed_write(ctx: &SyncContext, rel: &str) -> bool {
    ctx.suppress_writes.lock().unwrap().remove(rel)
}

#[derive(Debug)]
pub enum PushError {
    Conflict { body: String, mtime: f64 },
    Other(String),
}

#[derive(Deserialize)]
struct FileResponse {
    body: String,
    mtime: f64,
}

#[derive(Deserialize)]
struct ManifestEntry {
    path: String,
    mtime: f64,
}

/// Mirrors sync_routes.py's Python-side SyncFileWriteRequest model 1:1 --
/// was previously an ad hoc serde_json::json!() Value even though the
/// *response* to this same call (FileResponse, above) and the GET side
/// (ManifestEntry, above) already use typed structs.
#[derive(Serialize)]
struct SyncFileWriteRequest<'a> {
    path: &'a str,
    body: &'a str,
    expected_mtime: Option<f64>,
}

/// sync_routes.py always raises this 409 via
/// `HTTPException(status_code=409, detail={"body": ..., "mtime": ...})`,
/// which FastAPI always wraps as `{"detail": {...}}` -- so this is the one
/// real shape the server ever sends, not a Value navigated defensively.
#[derive(Deserialize)]
struct ConflictDetail {
    body: String,
    mtime: f64,
}

#[derive(Deserialize)]
struct ConflictResponse {
    detail: ConflictDetail,
}

fn auth_header(ctx: &SyncContext) -> Option<String> {
    ctx.token.lock().unwrap().clone().map(|t| format!("Bearer {t}"))
}

pub async fn fetch_manifest(ctx: &SyncContext) -> Result<Vec<RemoteEntry>, String> {
    let mut req = ctx.http.get(format!("{}/sync/manifest", ctx.server_url));
    if let Some(h) = auth_header(ctx) {
        req = req.header("Authorization", h);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("manifest fetch failed: {}", resp.status()));
    }
    let entries: Vec<ManifestEntry> = resp.json().await.map_err(|e| e.to_string())?;
    Ok(entries.into_iter().map(|e| RemoteEntry { path: e.path, mtime: e.mtime }).collect())
}

pub async fn push_file(
    ctx: &SyncContext,
    rel: &str,
    body: &str,
    expected_mtime: Option<f64>,
) -> Result<f64, PushError> {
    let mut req = ctx
        .http
        .put(format!("{}/sync/file", ctx.server_url))
        .header("X-Sync-Client-Id", &ctx.client_id)
        .json(&SyncFileWriteRequest { path: rel, body, expected_mtime });
    if let Some(h) = auth_header(ctx) {
        req = req.header("Authorization", h);
    }
    let resp = req.send().await.map_err(|e| PushError::Other(e.to_string()))?;

    if resp.status() == reqwest::StatusCode::CONFLICT {
        let parsed: ConflictResponse = resp.json().await.map_err(|e| PushError::Other(e.to_string()))?;
        return Err(PushError::Conflict { body: parsed.detail.body, mtime: parsed.detail.mtime });
    }
    if !resp.status().is_success() {
        return Err(PushError::Other(format!("push failed: {}", resp.status())));
    }
    let parsed: FileResponse = resp.json().await.map_err(|e| PushError::Other(e.to_string()))?;
    Ok(parsed.mtime)
}

pub async fn pull_file(ctx: &SyncContext, rel: &str) -> Result<Option<(String, f64)>, String> {
    let mut req = ctx.http.get(format!("{}/sync/file", ctx.server_url)).query(&[("path", rel)]);
    if let Some(h) = auth_header(ctx) {
        req = req.header("Authorization", h);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(format!("pull failed: {}", resp.status()));
    }
    let parsed: FileResponse = resp.json().await.map_err(|e| e.to_string())?;
    Ok(Some((parsed.body, parsed.mtime)))
}

pub async fn delete_remote(ctx: &SyncContext, rel: &str) -> Result<(), String> {
    let mut req = ctx
        .http
        .delete(format!("{}/sync/file", ctx.server_url))
        .header("X-Sync-Client-Id", &ctx.client_id)
        .query(&[("path", rel)]);
    if let Some(h) = auth_header(ctx) {
        req = req.header("Authorization", h);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() && resp.status() != reqwest::StatusCode::NOT_FOUND {
        return Err(format!("delete failed: {}", resp.status()));
    }
    Ok(())
}

/// LWW conflict resolution when a push hit a 409: whichever mtime is newer
/// wins. Before overwriting the losing side, it's preserved as a
/// `<path>.conflict-<ts>.md` sibling (Dropbox/Drive-style conflicted copy)
/// rather than silently discarded — see the vault-sync plan's rationale.
pub async fn resolve_conflict_push_side(
    ctx: &Arc<SyncContext>,
    rel: &str,
    local_body: &str,
    server_body: String,
    server_mtime: f64,
) {
    let local_path = ctx.vault_path.join(rel);
    let local_mtime = std::fs::metadata(&local_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);

    if local_mtime >= server_mtime {
        write_conflict_copy(&local_path, &server_body, server_mtime);
        match push_file(ctx, rel, local_body, Some(server_mtime)).await {
            Ok(new_mtime) => {
                let mut state = ctx.state.lock().unwrap();
                state.files.insert(rel.to_string(), TrackedFile { last_synced_mtime: new_mtime, content_hash: content_hash(local_body) });
                save_sync_state(&state);
            }
            // Retrying with the server's own reported mtime and still
            // getting a 409 back means something's genuinely wrong (server
            // clock/storage weirdness, or a third writer) -- not a case the
            // next debounced fs event will resolve on its own the way a
            // plain PushError::Other might, so it's worth its own log line.
            Err(PushError::Conflict { mtime: retry_mtime, .. }) => {
                eprintln!(
                    "prisma-desktop sync: {rel} still conflicts after retrying with the server's own mtime ({retry_mtime}, expected {server_mtime})"
                );
            }
            Err(PushError::Other(reason)) => {
                eprintln!("prisma-desktop sync: retry push {rel} failed: {reason}");
            }
        }
    } else {
        write_conflict_copy(&local_path, local_body, local_mtime);
        mark_suppressed_write(ctx, rel);
        let hash = content_hash(&server_body);
        let _ = std::fs::write(&local_path, &server_body);
        let mut state = ctx.state.lock().unwrap();
        state.files.insert(rel.to_string(), TrackedFile { last_synced_mtime: server_mtime, content_hash: hash });
        save_sync_state(&state);
    }
}

fn write_conflict_copy(original: &Path, losing_body: &str, losing_mtime: f64) {
    let ts = losing_mtime as i64;
    let conflict_path = original.with_extension(format!("conflict-{ts}.md"));
    let _ = std::fs::write(conflict_path, losing_body);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn relative_md_path_accepts_plain_md_file() {
        let vault = Path::new("/home/user/vault");
        let abs = Path::new("/home/user/vault/notes/foo.md");
        assert_eq!(relative_md_path(vault, abs), Some("notes/foo.md".to_string()));
    }

    #[test]
    fn relative_md_path_rejects_non_md() {
        let vault = Path::new("/home/user/vault");
        let abs = Path::new("/home/user/vault/notes/foo.txt");
        assert_eq!(relative_md_path(vault, abs), None);
    }

    #[test]
    fn relative_md_path_rejects_skip_dirs() {
        let vault = Path::new("/home/user/vault");
        assert_eq!(relative_md_path(vault, Path::new("/home/user/vault/.git/foo.md")), None);
        assert_eq!(relative_md_path(vault, Path::new("/home/user/vault/node_modules/foo.md")), None);
        assert_eq!(relative_md_path(vault, Path::new("/home/user/vault/.hidden/foo.md")), None);
    }

    #[test]
    fn relative_md_path_rejects_outside_vault() {
        let vault = Path::new("/home/user/vault");
        let abs = Path::new("/home/user/elsewhere/foo.md");
        assert_eq!(relative_md_path(vault, abs), None);
    }

    #[test]
    fn relative_md_path_accepts_stream_yaml() {
        let vault = Path::new("/home/user/vault");
        let abs = Path::new("/home/user/vault/streams/my-research-topic.yaml");
        assert_eq!(relative_md_path(vault, abs), Some("streams/my-research-topic.yaml".to_string()));
    }

    #[test]
    fn relative_md_path_rejects_yaml_outside_streams_dir() {
        let vault = Path::new("/home/user/vault");
        assert_eq!(relative_md_path(vault, Path::new("/home/user/vault/notes/foo.yaml")), None);
        assert_eq!(relative_md_path(vault, Path::new("/home/user/vault/config.yaml")), None);
    }

    #[test]
    fn relative_md_path_rejects_non_yaml_inside_streams_dir() {
        let vault = Path::new("/home/user/vault");
        assert_eq!(relative_md_path(vault, Path::new("/home/user/vault/streams/notes.txt")), None);
    }

    #[test]
    fn relative_md_path_rejects_conflict_copies() {
        // Regression test for the 2026-07-25 exponential-duplication bug:
        // write_conflict_copy() creates files like this directly in the
        // vault, and they must never be re-scoped as syncable content.
        let vault = Path::new("/home/user/vault");
        assert_eq!(
            relative_md_path(vault, Path::new("/home/user/vault/notes/foo.conflict-1784954960.md")),
            None
        );
        // Nested conflict copies (what actually happened) must also be rejected.
        assert_eq!(
            relative_md_path(
                vault,
                Path::new("/home/user/vault/notes/foo.conflict-1784954960.conflict-1785014396.md")
            ),
            None
        );
    }

    #[test]
    fn content_hash_is_deterministic_and_content_sensitive() {
        assert_eq!(content_hash("hello"), content_hash("hello"));
        assert_ne!(content_hash("hello"), content_hash("hello!"));
        // Known SHA256("hello") — pins the algorithm/encoding, not just
        // "some hash function was applied."
        assert_eq!(
            content_hash("hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    // pub(crate) so sibling modules' own tests (pull.rs) can build a
    // SyncContext the same way instead of a second near-duplicate helper.
    pub(crate) fn test_ctx() -> SyncContext {
        SyncContext {
            http: reqwest::Client::new(),
            server_url: "http://example.invalid".into(),
            vault_path: PathBuf::from("/tmp/does-not-matter"),
            client_id: "test-client".into(),
            token: Mutex::new(None),
            state: Mutex::new(SyncState::default()),
            suppress_writes: Mutex::new(HashSet::new()),
            ws_outbound: Mutex::new(None),
            needs_reauth: AtomicBool::new(false),
        }
    }

    /// Minimal single-response mock HTTP server for tests exercising
    /// push_file/pull_file/fetch_manifest/delete_remote/
    /// resolve_conflict_push_side -- without adding a mocking crate as a
    /// new dependency. Fully drains the incoming request (headers + any
    /// Content-Length body) before responding: closing a TcpStream while
    /// the kernel still has unread inbound bytes buffered can trigger a
    /// TCP RST instead of a clean close, which reqwest surfaces as
    /// "received unexpected message from connection" -- confirmed live,
    /// this was the actual cause the first version of this helper hit
    /// (naively assuming the client's small write would complete
    /// regardless of whether the server ever read it -- true for the
    /// connection to be established, not true for closing it cleanly
    /// afterward). pub(crate) so pull.rs's/push.rs's own tests can reuse it.
    pub(crate) fn spawn_mock_response(status: u16, body: &str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let body = body.to_string();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                use std::io::{BufRead, BufReader, Write};
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut content_length: usize = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break; // connection closed before headers finished
                    }
                    let trimmed = line.trim_end();
                    if trimmed.is_empty() {
                        break; // blank line = end of headers
                    }
                    if let Some(v) = trimmed.strip_prefix("Content-Length:").or_else(|| trimmed.strip_prefix("content-length:")) {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
                if content_length > 0 {
                    let mut discard = vec![0u8; content_length];
                    let _ = std::io::Read::read_exact(&mut reader, &mut discard);
                }

                let status_text = match status {
                    200 => "OK",
                    201 => "Created",
                    404 => "Not Found",
                    409 => "Conflict",
                    _ => "Error",
                };
                let resp = format!(
                    "HTTP/1.1 {status} {status_text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body,
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });
        format!("http://{addr}")
    }

    /// Accepts a connection and then does nothing at all -- no response,
    /// connection left open -- the exact shape of the real incident this
    /// module's build_http_client() comment describes (a pod dying
    /// mid-request during a rolling deploy). `_listener` must stay alive
    /// for the returned address to keep accepting; the caller holds it.
    fn spawn_mock_hang() -> (String, std::net::TcpListener) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let accept_listener = listener.try_clone().unwrap();
        std::thread::spawn(move || {
            let _ = accept_listener.accept(); // hold the connection open, write nothing, ever
            std::thread::sleep(std::time::Duration::from_secs(60));
        });
        (format!("http://{addr}"), listener)
    }

    #[test]
    fn build_http_client_times_out_on_a_connection_that_never_responds() {
        // Confirmed live 2026-08-27: the desktop app froze completely --
        // even UI reload stopped working -- after several rolling restarts
        // of the prisma server it syncs against. Root cause: reqwest::
        // Client::new() (no timeout) awaited a response from a connection
        // whose server had died mid-request, forever. This is the fix,
        // verified against the same failure shape: a connection accepted
        // but never answered must actually fail, not hang.
        let (url, _listener) = spawn_mock_hang();
        let client = build_http_client_with_timeout(std::time::Duration::from_millis(200));

        // A plain #[test] has no ambient Tokio runtime -- tauri::async_
        // runtime::block_on relies on one already existing (set up by
        // tauri::Builder::run() in the real app), which a unit test never
        // does. reqwest's own .timeout() needs the time driver specifically
        // (unlike the other tests in this file, which never hit that gap
        // because they don't exercise a timeout), so this needs a real,
        // self-contained runtime rather than reusing that helper.
        // client.get(&url).send() must be constructed *inside* the async
        // block, not passed in as an already-evaluated argument -- reqwest
        // starts its internal tokio::time::sleep(timeout) as soon as
        // .send() is called (not lazily on first poll), which needs an
        // active runtime context at that exact point, not just whenever
        // the resulting future later gets polled/awaited.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let started = std::time::Instant::now();
        let result = rt.block_on(async { client.get(&url).send().await });

        assert!(result.is_err(), "a connection that never responds must time out, not hang");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "took {:?} -- the 200ms timeout did not actually bound the wait",
            started.elapsed()
        );
    }

    #[test]
    fn write_conflict_copy_writes_losing_body_to_conflict_sibling() {
        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("notes")).unwrap();
        let original = dir.join("notes/a.md");
        std::fs::write(&original, "current content").unwrap();

        write_conflict_copy(&original, "losing content", 1_700_000_000.0);

        let conflict_path = dir.join("notes/a.conflict-1700000000.md");
        assert!(conflict_path.exists());
        assert_eq!(std::fs::read_to_string(&conflict_path).unwrap(), "losing content");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_conflict_push_side_server_wins_overwrites_local_and_writes_conflict_copy() {
        // The server-wins branch never touches the network (only the
        // local-wins branch retries push_file), so this exercises the real
        // resolve_conflict_push_side end to end without needing an HTTP
        // mock -- covering the actual "don't lose the loser's edit"
        // mechanism this function exists for, previously exercised only by
        // #[ignore]'d manual tests against a live server.
        let _guard = crate::json_store::TEST_ENV_GUARD.lock().unwrap();
        let config_dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}-cfg", uuid::Uuid::new_v4()));
        std::env::set_var("PRISMA_DESKTOP_CONFIG_DIR", &config_dir);

        let vault_dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}-vault", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(vault_dir.join("notes")).unwrap();
        let local_path = vault_dir.join("notes/a.md");
        std::fs::write(&local_path, "local content").unwrap();
        // The server-wins branch names the conflict copy after the LOSER's
        // (local's) own real on-disk mtime, not server_mtime -- read it the
        // same way the source does so the test doesn't have to guess it.
        let local_mtime = std::fs::metadata(&local_path)
            .and_then(|m| m.modified())
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();

        let ctx = Arc::new(SyncContext { vault_path: vault_dir.clone(), ..test_ctx() });

        // Deliberately far in the future so it's always newer than the
        // just-created local file's real mtime -- avoids needing a
        // filetime-setting crate just to control this in a test.
        let server_mtime = 9_999_999_999.0;
        let server_body = "server content".to_string();

        tauri::async_runtime::block_on(resolve_conflict_push_side(
            &ctx, "notes/a.md", "local content", server_body.clone(), server_mtime,
        ));

        std::env::remove_var("PRISMA_DESKTOP_CONFIG_DIR");

        // local file overwritten with the server's content
        assert_eq!(std::fs::read_to_string(&local_path).unwrap(), server_body);

        // the loser (local content) preserved as a conflict copy, not discarded
        let conflict_path = vault_dir.join(format!("notes/a.conflict-{}.md", local_mtime as i64));
        assert!(conflict_path.exists());
        assert_eq!(std::fs::read_to_string(&conflict_path).unwrap(), "local content");

        // state updated to reflect the server's version as now-synced
        {
            let state = ctx.state.lock().unwrap();
            let tracked = state.files.get("notes/a.md").expect("tracked entry for notes/a.md");
            assert_eq!(tracked.last_synced_mtime, server_mtime);
            assert_eq!(tracked.content_hash, content_hash(&server_body));
        }

        // the write was marked suppressed so the fs watcher doesn't echo
        // this overwrite back to the server as a push
        assert!(consume_suppressed_write(&ctx, "notes/a.md"));

        std::fs::remove_dir_all(&vault_dir).ok();
        std::fs::remove_dir_all(&config_dir).ok();
    }

    // ── push_file / pull_file / fetch_manifest / delete_remote ──────────
    // Previously covered only by #[ignore]'d manual tests against a live
    // server (see manual_check_against_real_server below). spawn_mock_response
    // makes these runnable as part of the normal automated suite.

    // NOTE: SyncContext is built inside each block_on'd async block below,
    // not before it -- see spawn_mock_response's doc comment for the real
    // bug this avoided (server must drain the request before closing).

    #[test]
    fn push_file_success_returns_new_mtime() {
        let server_url = spawn_mock_response(200, r#"{"body":"ignored","mtime":123.0}"#);

        let result = tauri::async_runtime::block_on(async move {
            let ctx = SyncContext { server_url, ..test_ctx() };
            push_file(&ctx, "notes/a.md", "hello", None).await
        });

        assert_eq!(result.unwrap(), 123.0);
    }

    #[test]
    fn push_file_conflict_returns_server_body_and_mtime() {
        let server_url = spawn_mock_response(
            409,
            r#"{"detail":{"body":"server content","mtime":150.0}}"#,
        );

        let result = tauri::async_runtime::block_on(async move {
            let ctx = SyncContext { server_url, ..test_ctx() };
            push_file(&ctx, "notes/a.md", "local content", Some(100.0)).await
        });

        match result {
            Err(PushError::Conflict { body, mtime }) => {
                assert_eq!(body, "server content");
                assert_eq!(mtime, 150.0);
            }
            other => panic!("expected PushError::Conflict, got {other:?}"),
        }
    }

    #[test]
    fn push_file_server_error_returns_other() {
        let server_url = spawn_mock_response(500, "");

        let result = tauri::async_runtime::block_on(async move {
            let ctx = SyncContext { server_url, ..test_ctx() };
            push_file(&ctx, "notes/a.md", "hello", None).await
        });

        assert!(matches!(result, Err(PushError::Other(_))));
    }

    #[test]
    fn pull_file_success_returns_body_and_mtime() {
        let server_url = spawn_mock_response(200, r#"{"body":"pulled content","mtime":200.0}"#);

        let result = tauri::async_runtime::block_on(async move {
            let ctx = SyncContext { server_url, ..test_ctx() };
            pull_file(&ctx, "notes/a.md").await
        });

        assert_eq!(result.unwrap(), Some(("pulled content".to_string(), 200.0)));
    }

    #[test]
    fn pull_file_not_found_returns_ok_none() {
        let server_url = spawn_mock_response(404, "");

        let result = tauri::async_runtime::block_on(async move {
            let ctx = SyncContext { server_url, ..test_ctx() };
            pull_file(&ctx, "notes/gone.md").await
        });

        assert_eq!(result.unwrap(), None);
    }

    #[test]
    fn fetch_manifest_parses_entries() {
        let server_url = spawn_mock_response(200, r#"[{"path":"notes/a.md","mtime":100.0},{"path":"notes/b.md","mtime":200.0}]"#);

        let result = tauri::async_runtime::block_on(async move {
            let ctx = SyncContext { server_url, ..test_ctx() };
            fetch_manifest(&ctx).await
        }).unwrap();

        assert_eq!(result, vec![
            RemoteEntry { path: "notes/a.md".into(), mtime: 100.0 },
            RemoteEntry { path: "notes/b.md".into(), mtime: 200.0 },
        ]);
    }

    #[test]
    fn delete_remote_succeeds_on_200() {
        let server_url = spawn_mock_response(200, r#"{"status":"deleted"}"#);

        let result = tauri::async_runtime::block_on(async move {
            let ctx = SyncContext { server_url, ..test_ctx() };
            delete_remote(&ctx, "notes/a.md").await
        });

        assert!(result.is_ok());
    }

    #[test]
    fn delete_remote_treats_404_as_success() {
        // Already gone server-side -- same end state as a successful
        // delete, not an error worth surfacing.
        let server_url = spawn_mock_response(404, "");

        let result = tauri::async_runtime::block_on(async move {
            let ctx = SyncContext { server_url, ..test_ctx() };
            delete_remote(&ctx, "notes/a.md").await
        });

        assert!(result.is_ok());
    }

    #[test]
    fn resolve_conflict_push_side_local_wins_retries_push_with_server_mtime() {
        let _guard = crate::json_store::TEST_ENV_GUARD.lock().unwrap();
        let config_dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}-cfg", uuid::Uuid::new_v4()));
        std::env::set_var("PRISMA_DESKTOP_CONFIG_DIR", &config_dir);

        let vault_dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}-vault", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(vault_dir.join("notes")).unwrap();
        let local_path = vault_dir.join("notes/a.md");
        std::fs::write(&local_path, "local content").unwrap();

        // Local wins: the just-created file's real on-disk mtime ("now") is
        // always >= this deliberately-far-in-the-past server_mtime.
        let server_mtime = 100.0;
        let server_body = "server content".to_string();
        // The retry push (with the server's own mtime as expected_mtime)
        // succeeds this time, reported back as this new mtime. A clean,
        // exactly-representable literal -- not the real, ultra-precise
        // filesystem mtime -- avoids hand-formatting a float into this
        // mock's JSON text via `{}` Display hitting the exact
        // precision-boundary class of bug this project's own sync
        // protocol had to fix once already (see sync_routes.py's
        // _MTIME_TOLERANCE_SECONDS): reqwest/serde_json round-trip a
        // float exactly, but format!("{}", x) doesn't always reproduce
        // every significant digit float that Display would need to.
        let retry_mtime = 555.0;
        let server_url = spawn_mock_response(200, &format!(r#"{{"body":"ignored","mtime":{retry_mtime}}}"#));
        let vault_path_for_ctx = vault_dir.clone();
        let server_body_for_ctx = server_body.clone();

        let ctx = tauri::async_runtime::block_on(async move {
            let ctx = Arc::new(SyncContext { vault_path: vault_path_for_ctx, server_url, ..test_ctx() });
            resolve_conflict_push_side(&ctx, "notes/a.md", "local content", server_body_for_ctx, server_mtime).await;
            ctx
        });

        std::env::remove_var("PRISMA_DESKTOP_CONFIG_DIR");

        // local content is retained (local won) -- NOT overwritten with the server's
        assert_eq!(std::fs::read_to_string(&local_path).unwrap(), "local content");

        // the server's now-discarded version is preserved as the conflict copy
        let conflict_path = vault_dir.join(format!("notes/a.conflict-{}.md", server_mtime as i64));
        assert!(conflict_path.exists());
        assert_eq!(std::fs::read_to_string(&conflict_path).unwrap(), server_body);

        // state updated to reflect the successful retry push
        let state = ctx.state.lock().unwrap();
        let tracked = state.files.get("notes/a.md").expect("tracked entry for notes/a.md");
        assert_eq!(tracked.last_synced_mtime, retry_mtime);
        assert_eq!(tracked.content_hash, content_hash("local content"));

        std::fs::remove_dir_all(&vault_dir).ok();
        std::fs::remove_dir_all(&config_dir).ok();
    }

    #[test]
    fn suppressed_write_is_consumed_exactly_once() {
        let ctx = test_ctx();
        mark_suppressed_write(&ctx, "notes/a.md");
        assert!(consume_suppressed_write(&ctx, "notes/a.md"));
        assert!(!consume_suppressed_write(&ctx, "notes/a.md"));
    }

    #[test]
    fn unsuppressed_path_is_not_consumed() {
        let ctx = test_ctx();
        assert!(!consume_suppressed_write(&ctx, "notes/never-marked.md"));
    }

    #[test]
    fn suppression_is_per_path() {
        let ctx = test_ctx();
        mark_suppressed_write(&ctx, "notes/a.md");
        assert!(!consume_suppressed_write(&ctx, "notes/b.md"));
        assert!(consume_suppressed_write(&ctx, "notes/a.md"));
    }

    #[test]
    fn detect_same_filesystem_returns_false_and_cleans_up_when_server_unreachable() {
        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}-fsprobe", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let http = reqwest::Client::new();
        // Port 1 is privileged/never listening in any test environment --
        // same "real connection-refused case" convention as window.rs's
        // is_server_reachable test.
        let same_fs = tauri::async_runtime::block_on(detect_same_filesystem(&http, "http://127.0.0.1:1", &dir));

        assert!(!same_fs);
        let leftover: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert!(leftover.is_empty(), "probe file was not cleaned up: {leftover:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn server_vault_root_returns_none_when_server_unreachable() {
        let http = reqwest::Client::new();
        let root = tauri::async_runtime::block_on(server_vault_root(&http, "http://127.0.0.1:1"));
        assert_eq!(root, None);
    }

    /// Manual, one-off check against a REAL running `prisma serve`
    /// (server.auth.mode: password) — not part of the automated suite.
    /// Exercises the actual wire format (JSON field names, header names,
    /// Authorization: Bearer) against the real Python server, which the
    /// pure-logic tests above can't catch. Run with:
    ///   PRISMA_TEST_SERVER_URL=http://127.0.0.1:18765 \
    ///   PRISMA_TEST_PASSWORD=manualtest123 \
    ///   cargo test --lib sync::tests::manual_check_against_real_server -- --ignored --nocapture
    #[test]
    #[ignore]
    fn manual_check_against_real_server() {
        let server_url = std::env::var("PRISMA_TEST_SERVER_URL").expect("set PRISMA_TEST_SERVER_URL");
        let password = std::env::var("PRISMA_TEST_PASSWORD").expect("set PRISMA_TEST_PASSWORD");

        tauri::async_runtime::block_on(async move {
            let http = reqwest::Client::new();

            let login_resp = http
                .post(format!("{server_url}/auth/login"))
                .json(&serde_json::json!({ "password": password }))
                .send()
                .await
                .expect("login request failed");
            assert!(login_resp.status().is_success(), "login failed: {}", login_resp.status());
            let login_body: serde_json::Value = login_resp.json().await.unwrap();
            let token = login_body["token"].as_str().unwrap().to_string();
            println!("login ok, token prefix: {}...", &token[..12.min(token.len())]);

            let ctx = SyncContext {
                http,
                server_url: server_url.clone(),
                vault_path: std::env::temp_dir(),
                client_id: "manual-check-client".into(),
                token: Mutex::new(Some(token)),
                state: Mutex::new(SyncState::default()),
                suppress_writes: Mutex::new(HashSet::new()),
                ws_outbound: Mutex::new(None),
                needs_reauth: AtomicBool::new(false),
            };

            let manifest = fetch_manifest(&ctx).await.expect("manifest fetch failed");
            println!("manifest has {} entries", manifest.len());

            let test_path = "notes/_manual_sync_check.md";

            // Clean slate: delete first in case a previous run left it behind.
            let _ = delete_remote(&ctx, test_path).await;

            let mtime = push_file(&ctx, test_path, "hello from rust", None)
                .await
                .map_err(|e| match e {
                    PushError::Conflict { body, mtime } => format!("unexpected conflict: {body} @ {mtime}"),
                    PushError::Other(s) => s,
                })
                .expect("push failed");
            println!("pushed at mtime {mtime}");

            let (body, pulled_mtime) = pull_file(&ctx, test_path)
                .await
                .expect("pull request failed")
                .expect("pull returned None for a file we just pushed");
            assert_eq!(body, "hello from rust");
            assert_eq!(pulled_mtime, mtime);
            println!("pulled back matching content and mtime");

            // Conflict path: push again with a deliberately wrong expected_mtime.
            let conflict_err = push_file(&ctx, test_path, "conflicting write", Some(1.0))
                .await
                .err()
                .expect("expected a 409 conflict");
            match conflict_err {
                PushError::Conflict { body, mtime } => {
                    assert_eq!(body, "hello from rust");
                    assert_eq!(mtime, pulled_mtime);
                    println!("409 conflict correctly returned server's current state");
                }
                PushError::Other(s) => panic!("expected Conflict, got Other({s})"),
            }

            delete_remote(&ctx, test_path).await.expect("delete failed");
            let after = pull_file(&ctx, test_path).await.expect("pull after delete failed");
            assert!(after.is_none(), "file should be gone after delete");
            println!("delete confirmed — cleanup complete");
        });
    }

    /// Manual: same server as above, but exercises the /ws subprotocol
    /// handshake and echo-loop suppression (exclude_client_id) — the two
    /// trickiest cross-language pieces of the pull path. Run with the same
    /// env vars as manual_check_against_real_server.
    #[test]
    #[ignore]
    fn manual_check_ws_subprotocol_and_echo_suppression() {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::Message;

        let server_url = std::env::var("PRISMA_TEST_SERVER_URL").expect("set PRISMA_TEST_SERVER_URL");
        let password = std::env::var("PRISMA_TEST_PASSWORD").expect("set PRISMA_TEST_PASSWORD");

        tauri::async_runtime::block_on(async move {
            let http = reqwest::Client::new();
            let login_body: serde_json::Value = http
                .post(format!("{server_url}/auth/login"))
                .json(&serde_json::json!({ "password": password }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let token = login_body["token"].as_str().unwrap().to_string();

            // Clean slate BEFORE connecting the WS listener below — this
            // delete's own broadcast must not be mistaken for the push's.
            let cleanup_ctx = SyncContext {
                http: http.clone(),
                server_url: server_url.clone(),
                vault_path: std::env::temp_dir(),
                client_id: "different-pusher".into(),
                token: Mutex::new(Some(token.clone())),
                state: Mutex::new(SyncState::default()),
                suppress_writes: Mutex::new(HashSet::new()),
                ws_outbound: Mutex::new(None),
                needs_reauth: AtomicBool::new(false),
            };
            let _ = delete_remote(&cleanup_ctx, "notes/_manual_ws_check.md").await;

            let ws_url = format!(
                "{}/ws?client_id=ws-listener",
                server_url.replacen("http://", "ws://", 1)
            );
            let mut request = ws_url.into_client_request().unwrap();
            request
                .headers_mut()
                .insert("Sec-WebSocket-Protocol", format!("bearer, {token}").parse().unwrap());

            let (ws_stream, response) = tokio_tungstenite::connect_async(request).await.expect("ws connect failed");
            assert_eq!(
                response.headers().get("sec-websocket-protocol").map(|v| v.to_str().unwrap()),
                Some("bearer"),
                "server must echo the 'bearer' subprotocol"
            );
            println!("ws connected, subprotocol confirmed");
            let (_write, mut read) = ws_stream.split();

            let ctx = cleanup_ctx;
            let test_path = "notes/_manual_ws_check.md";
            push_file(&ctx, test_path, "ws test content", None).await.expect("push failed");

            let msg = tokio::time::timeout(std::time::Duration::from_secs(5), read.next())
                .await
                .expect("timed out waiting for vault_change broadcast")
                .expect("stream ended")
                .expect("ws error");
            if let Message::Text(text) = msg {
                println!("received: {text}");
                let event: serde_json::Value = serde_json::from_str(&text).unwrap();
                assert_eq!(event["type"], "vault_change");
                assert_eq!(event["action"], "sync_write");
                assert_eq!(event["path"], test_path);
            } else {
                panic!("expected a text message, got {msg:?}");
            }

            delete_remote(&ctx, test_path).await.expect("cleanup delete failed");
            println!("echo-loop exclusion confirmed: a push from a DIFFERENT client_id was correctly delivered");
        });
    }

    /// Manual: the other half of echo-loop suppression — a push using the
    /// SAME client_id as the WS listener must NOT be delivered back to it.
    #[test]
    #[ignore]
    fn manual_check_ws_same_client_id_is_not_echoed() {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let server_url = std::env::var("PRISMA_TEST_SERVER_URL").expect("set PRISMA_TEST_SERVER_URL");
        let password = std::env::var("PRISMA_TEST_PASSWORD").expect("set PRISMA_TEST_PASSWORD");

        tauri::async_runtime::block_on(async move {
            let http = reqwest::Client::new();
            let login_body: serde_json::Value = http
                .post(format!("{server_url}/auth/login"))
                .json(&serde_json::json!({ "password": password }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let token = login_body["token"].as_str().unwrap().to_string();

            let shared_client_id = "same-client-echo-test";
            let ctx = SyncContext {
                http: http.clone(),
                server_url: server_url.clone(),
                vault_path: std::env::temp_dir(),
                client_id: shared_client_id.into(),
                token: Mutex::new(Some(token.clone())),
                state: Mutex::new(SyncState::default()),
                suppress_writes: Mutex::new(HashSet::new()),
                ws_outbound: Mutex::new(None),
                needs_reauth: AtomicBool::new(false),
            };
            let test_path = "notes/_manual_ws_echo_check.md";
            let _ = delete_remote(&ctx, test_path).await;

            let ws_url = format!(
                "{}/ws?client_id={shared_client_id}",
                server_url.replacen("http://", "ws://", 1)
            );
            let mut request = ws_url.into_client_request().unwrap();
            request
                .headers_mut()
                .insert("Sec-WebSocket-Protocol", format!("bearer, {token}").parse().unwrap());
            let (ws_stream, _response) = tokio_tungstenite::connect_async(request).await.expect("ws connect failed");
            let (_write, mut read) = ws_stream.split();

            push_file(&ctx, test_path, "should not echo", None).await.expect("push failed");

            let result = tokio::time::timeout(std::time::Duration::from_secs(2), read.next()).await;
            assert!(
                result.is_err(),
                "expected no message within 2s (own push shouldn't echo back), but got: {result:?}"
            );
            println!("confirmed: a push using the SAME client_id as the WS listener was NOT echoed back");

            delete_remote(&ctx, test_path).await.expect("cleanup delete failed");
        });
    }
}

// ── Tauri commands / engine lifecycle ─────────────────────────────────────────

pub struct EngineHandle {
    ctx: Arc<SyncContext>,
    _watcher: push::WatcherHandle,
    stop: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct AppState {
    engine: Mutex<Option<EngineHandle>>,
}

/// Fetches the server's own reported vault root (GET /status's vault.root,
/// see prisma's SystemStatusResponse) -- used only as the cheap first half
/// of the same-filesystem check below. None on any failure (unreachable,
/// bad JSON, etc.) -- callers treat that as "can't confirm a collision",
/// never as a reason to block starting sync.
async fn server_vault_root(http: &reqwest::Client, server_url: &str) -> Option<String> {
    let resp = http.get(format!("{server_url}/status")).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: serde_json::Value = resp.json().await.ok()?;
    json.get("vault")?.get("root")?.as_str().map(str::to_string)
}

/// The deeper check `isLoopbackUrl` (prisma/ui/src/lib/sync.ts) alone
/// misses: a hostname that isn't 127.0.0.1/localhost but still routes back
/// to this same machine (a LAN hostname, a router hairpin, etc.) would
/// otherwise start a vault-sync engine that pushes/pulls a directory
/// against itself. Writes a random marker file straight to local disk
/// (bypassing the sync protocol entirely, so no push has happened yet) and
/// asks the server to read that exact path back over /sync/file: if it's
/// the same filesystem the server sees it immediately with zero network
/// sync involved; if not, /sync/file 404s -- a fresh random UUID filename
/// can't coincidentally already exist on a genuinely different vault.
/// Always cleans up the marker file itself.
async fn detect_same_filesystem(http: &reqwest::Client, server_url: &str, vault_path: &Path) -> bool {
    let probe_name = format!(".prisma-desktop-fs-probe-{}.md", uuid::Uuid::new_v4());
    let probe_path = vault_path.join(&probe_name);

    if std::fs::write(&probe_path, "fs-identity-probe").is_err() {
        return false;
    }

    let same_fs = http
        .get(format!("{server_url}/sync/file"))
        .query(&[("path", probe_name.as_str())])
        .send()
        .await
        .ok()
        .is_some_and(|r| r.status().is_success());

    let _ = std::fs::remove_file(&probe_path);
    same_fs
}

/// Starts syncing using whatever's currently configured in Settings
/// (server_url, vault_path — falling back to the default data-dir location
/// per settings::resolve_vault_path if unset), same "sensible default,
/// override in Settings" convention as scale/server_url already follow.
#[tauri::command]
pub async fn sync_start(app: tauri::AppHandle) -> Result<(), String> {
    {
        let state = app.state::<AppState>();
        let guard = state.engine.lock().unwrap();
        if guard.is_some() {
            return Err("sync already running".into());
        }
    }

    let settings = crate::settings::load_settings();
    let server_url = settings.api_url();
    let server_url = server_url.trim_end_matches('/').to_string();
    let vault_path = crate::settings::resolve_vault_path(&settings);
    std::fs::create_dir_all(&vault_path).map_err(|e| e.to_string())?;

    let http = build_http_client();
    // Cheap string check first (isLoopbackUrl already screens out the
    // obvious 127.0.0.1 case in TS -- this covers a non-loopback hostname
    // that still happens to route back to this same machine); the
    // network+disk round trip in detect_same_filesystem only runs if the
    // paths already coincide, which should be rare.
    if let Some(remote_root) = server_vault_root(&http, &server_url).await {
        if remote_root == vault_path.display().to_string()
            && detect_same_filesystem(&http, &server_url, &vault_path).await
        {
            return Err(format!(
                "refusing to sync: the server's vault root ({remote_root}) looks like the same physical filesystem as this machine's local vault folder -- syncing would push and pull the same files against themselves. If this really is a separate server, point \"Local vault folder\" at a different path."
            ));
        }
    }

    let token = crate::auth::session_for(&server_url).map(|s| s.token);
    let ctx = Arc::new(SyncContext {
        http,
        server_url: server_url.clone(),
        vault_path,
        client_id: client_id(),
        token: Mutex::new(token),
        state: Mutex::new(load_sync_state()),
        suppress_writes: Mutex::new(HashSet::new()),
        ws_outbound: Mutex::new(None),
        needs_reauth: AtomicBool::new(false),
    });

    // No client-driven initial reconciliation anymore -- the server asks
    // for a full manifest (request_manifest) as soon as the WS connection
    // below is live, and decides push/pull/conflict per path from there.
    // See pull.rs's module doc comment for the full 2026-07-26 redesign
    // rationale (the previous client-decides-when-to-push design caused a
    // real, sustained duplicate-push loop with nothing to arbitrate it).
    let watcher = push::start_watcher(ctx.clone()).map_err(|e| e.to_string())?;
    let stop = Arc::new(AtomicBool::new(false));
    tauri::async_runtime::spawn(pull::run_pull_loop(ctx.clone(), stop.clone()));

    let state = app.state::<AppState>();
    let mut guard = state.engine.lock().unwrap();
    *guard = Some(EngineHandle { ctx, _watcher: watcher, stop });
    Ok(())
}

#[tauri::command]
pub fn sync_stop(app: tauri::AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mut guard = state.engine.lock().unwrap();
    if let Some(handle) = guard.take() {
        handle.stop.store(true, Ordering::SeqCst);
    }
    Ok(())
}

#[derive(Serialize)]
pub struct SyncStatusInfo {
    pub running: bool,
    pub server_url: Option<String>,
    pub vault_path: Option<String>,
    pub tracked_files: usize,
    /// See SyncContext::needs_reauth's doc comment -- true when the running
    /// engine's WS connection is being rejected as unauthorized (expired or
    /// invalid token) and needs a fresh sync_login to recover. Always false
    /// when `running` is false.
    pub needs_reauth: bool,
}

#[tauri::command]
pub fn sync_engine_status(app: tauri::AppHandle) -> SyncStatusInfo {
    let state = app.state::<AppState>();
    let guard = state.engine.lock().unwrap();
    match guard.as_ref() {
        Some(handle) => SyncStatusInfo {
            running: true,
            server_url: Some(handle.ctx.server_url.clone()),
            vault_path: Some(handle.ctx.vault_path.display().to_string()),
            tracked_files: handle.ctx.state.lock().unwrap().files.len(),
            needs_reauth: handle.ctx.needs_reauth.load(Ordering::SeqCst),
        },
        None => SyncStatusInfo {
            running: false, server_url: None, vault_path: None, tracked_files: 0, needs_reauth: false,
        },
    }
}

/// Hands a freshly (re-)authenticated token to the currently running
/// engine, if any, and only if it's running against the same server that
/// was just (re-)authenticated -- so sync_login/sync_logout (auth/mod.rs)
/// take effect immediately on a live connection instead of requiring the
/// user to stop/restart sync manually. A no-op if no engine is running, or
/// it's running against a different server_url.
pub fn update_running_token(app: &tauri::AppHandle, server_url: &str, token: Option<String>) {
    let state = app.state::<AppState>();
    let guard = state.engine.lock().unwrap();
    if let Some(handle) = guard.as_ref() {
        if handle.ctx.server_url.trim_end_matches('/') == server_url.trim_end_matches('/') {
            *handle.ctx.token.lock().unwrap() = token;
            handle.ctx.needs_reauth.store(false, Ordering::SeqCst);
        }
    }
}

#[derive(Serialize)]
pub struct SyncDiffInfo {
    pub push_new: usize,
    pub pull_new: usize,
    pub push_delete: usize,
    pub pull_delete: usize,
    pub pull_update: usize,
    pub push_recreate: usize,
    pub reachable: bool,
}

/// Computes the local-vs-server diff without applying any of it — lets the
/// UI show "what would sync do right now" (see the vault-sync plan's
/// tracked/untracked lifecycle table for what each count means) whether or
/// not the engine is currently running.
#[tauri::command]
pub async fn sync_diff(app: tauri::AppHandle) -> Result<SyncDiffInfo, String> {
    let settings = crate::settings::load_settings();
    let server_url = settings.api_url();
    let vault_path = crate::settings::resolve_vault_path(&settings);

    let token = crate::auth::session_for(&server_url).map(|s| s.token);
    let ctx = SyncContext {
        http: reqwest::Client::new(),
        server_url,
        vault_path: vault_path.clone(),
        client_id: "diff-check".into(),
        token: Mutex::new(token),
        state: Mutex::new(load_sync_state()),
        suppress_writes: Mutex::new(HashSet::new()),
        ws_outbound: Mutex::new(None),
        needs_reauth: AtomicBool::new(false),
    };

    // Reuse a running engine's live state if there is one — otherwise the
    // freshly-loaded sync_state.json above (same file on disk) is fine.
    {
        let app_state = app.state::<AppState>();
        let guard = app_state.engine.lock().unwrap();
        if let Some(handle) = guard.as_ref() {
            *ctx.state.lock().unwrap() = handle.ctx.state.lock().unwrap().clone();
        }
    }

    let local = manifest::walk_local_md(&vault_path);
    let remote = match fetch_manifest(&ctx).await {
        Ok(entries) => entries,
        Err(_) => {
            return Ok(SyncDiffInfo {
                push_new: 0, pull_new: 0, push_delete: 0, pull_delete: 0,
                pull_update: 0, push_recreate: 0, reachable: false,
            })
        }
    };
    let tracked = ctx.state.lock().unwrap().files.clone();
    let actions = manifest::reconcile(&local, &remote, &tracked);

    let mut info = SyncDiffInfo {
        push_new: 0, pull_new: 0, push_delete: 0, pull_delete: 0,
        pull_update: 0, push_recreate: 0, reachable: true,
    };
    for action in actions {
        match action {
            manifest::ReconcileAction::PushNew(_) => info.push_new += 1,
            manifest::ReconcileAction::PullNew(_) => info.pull_new += 1,
            manifest::ReconcileAction::PushDelete(_) => info.push_delete += 1,
            manifest::ReconcileAction::PullDelete(_) => info.pull_delete += 1,
            manifest::ReconcileAction::PullUpdate(_) => info.pull_update += 1,
            manifest::ReconcileAction::PushReCreate(_) => info.push_recreate += 1,
        }
    }
    Ok(info)
}
