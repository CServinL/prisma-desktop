//! Zotero Desktop's local connector API (port 23119) -- only reachable from
//! the same machine Zotero Desktop runs on. That's why this lives here, in
//! prisma-desktop, and not in the prisma server: prisma-server only ever
//! talks to the Zotero Web API, never this local connector, since the
//! server and the user's Zotero Desktop install are not guaranteed to be on
//! the same machine (confirmed 2026-07-27 -- prisma-server will never use
//! the Zotero Desktop API, only the Web API).
//!
//! Ported from prisma's services/zotero.py::_desktop_ping (removed
//! 2026-07-26 as dead code -- ZoteroMode.desktop was never reachable from
//! the server side) -- the one piece of that old code with real, working
//! logic. Its siblings, _desktop_collections/_desktop_items, were always
//! `raise NotImplementedError` stubs there; nothing to port for those --
//! Zotero's local connector API is designed for the "save to Zotero"
//! browser-extension flow (ping + save-item endpoints), not a general
//! list-collections/list-items read API, which is presumably why they were
//! never implemented in the first place.

use std::time::Duration;

async fn ping(url: &str, timeout: Duration) -> bool {
    let Ok(client) = reqwest::Client::builder().timeout(timeout).build() else {
        return false;
    };
    client.get(url).send().await.is_ok()
}

#[tauri::command]
pub async fn zotero_desktop_ping() -> bool {
    ping("http://127.0.0.1:23119/connector/ping", Duration::from_secs(2)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_returns_true_when_reachable() {
        let server_url = crate::sync::tests::spawn_mock_response(200, "");
        let reachable = tauri::async_runtime::block_on(async move {
            ping(&format!("{server_url}/connector/ping"), Duration::from_secs(2)).await
        });
        assert!(reachable);
    }

    #[test]
    fn ping_returns_false_when_unreachable() {
        // Nothing listening on this port.
        let reachable = tauri::async_runtime::block_on(async {
            ping("http://127.0.0.1:1/connector/ping", Duration::from_millis(200)).await
        });
        assert!(!reachable);
    }
}
