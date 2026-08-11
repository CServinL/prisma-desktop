use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use serde::{Deserialize, Serialize};

use crate::schema_gov::MigrationChain;

/// Bumped whenever Settings' shape changes in a way `#[serde(default)]`
/// alone can't handle (a rename, a type change) -- add the real migration
/// step to settings_migration_chain() at the same time. No real migration
/// has ever been needed yet; this is the same "infrastructure ready, chain
/// starts empty" posture prisma's Python schema_gov had before its first one.
pub const SETTINGS_SCHEMA_VERSION: u32 = 1;

fn settings_migration_chain() -> MigrationChain {
    MigrationChain { current_version: SETTINGS_SCHEMA_VERSION, migrations: HashMap::new() }
}

/// Guards every load-modify-save cycle against settings.json -- both
/// save_settings_cmd (below) and window.rs's debounced resize/move handler
/// do "load current, mutate a subset of fields, write the whole file back."
/// Without this, the two could interleave: e.g. a resize's load happening
/// between save_settings_cmd's load and its write would have the resize's
/// save undo whatever save_settings_cmd just persisted (or vice versa).
/// A single process-wide lock is simple and sufficient here -- these are
/// both low-frequency, user-triggered-or-debounced writes, not a hot path.
pub(crate) static SETTINGS_LOCK: Mutex<()> = Mutex::new(());

// Used by serde when deserializing a pre-versioning file with no
// schema_version key at all -- SETTINGS_SCHEMA_VERSION itself is what
// MigrationChain::migrate() stamps onto the JSON before this ever runs, in
// the normal load_settings() path; this default only matters for a struct
// literal built directly from JSON some other way.
fn default_schema_version() -> u32 { 1 }

fn default_hostname() -> String { "127.0.0.1".into() }
fn default_api_port() -> u16 { 8765 }
fn default_web_port() -> u16 { 8766 }
// Mirrors +page.svelte's SIDEBAR_DEFAULT_WIDTH -- kept in sync manually
// since the two aren't generated from one schema.
fn default_sidebar_width() -> u32 { 220 }

#[derive(Serialize, Deserialize, Clone)]
pub struct Settings {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub scale: f64,
    #[serde(default = "default_hostname")]
    pub hostname: String,
    #[serde(default)]
    pub tls: bool,
    #[serde(default = "default_api_port")]
    pub api_port: u16,
    #[serde(default = "default_web_port")]
    pub web_port: u16,
    pub window_width: Option<u32>,
    pub window_height: Option<u32>,
    pub window_x: Option<i32>,
    pub window_y: Option<i32>,
    pub window_maximized: Option<bool>,
    // Where the local vault .md copy lives. None until the user picks a
    // folder (or a first sync run falls back to a sensible default) — see
    // sync::manifest for how an unset path is handled at startup.
    pub vault_path: Option<String>,
    // Width in px of the resizable split between the nav pane and the main
    // viewport. Previously only lived in the webview's own localStorage
    // (origin-scoped -- resets whenever the configured hostname/port
    // changes, e.g. switching servers), unlike every other layout
    // preference here, which survives that.
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: u32,
}

impl Settings {
    pub fn scheme(&self) -> &'static str {
        if self.tls { "https" } else { "http" }
    }

    pub fn api_url(&self) -> String {
        format!("{}://{}:{}", self.scheme(), self.hostname, self.api_port)
    }

    pub fn web_url(&self) -> String {
        format!("{}://{}:{}", self.scheme(), self.hostname, self.web_port)
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // A fresh, no-file-yet Settings represents the CURRENT shape,
            // not v1 -- distinct from default_schema_version() above, which
            // only applies when deserializing an old file missing the key.
            schema_version: SETTINGS_SCHEMA_VERSION,
            // 1x reads as uncomfortably small on today's typical high-density
            // displays (confirmed live on a 4K monitor) — 1.5x is a more
            // usable out-of-the-box default for a fresh install with no
            // settings.json yet; still adjustable 1x-5x in Settings.
            scale: 1.5,
            hostname: default_hostname(),
            tls: false,
            api_port: default_api_port(),
            web_port: default_web_port(),
            window_width: None,
            window_height: None,
            window_x: None,
            window_y: None,
            window_maximized: None,
            vault_path: None,
            sidebar_width: default_sidebar_width(),
        }
    }
}

pub fn settings_path() -> PathBuf {
    crate::json_store::config_file_path("settings.json")
}

pub fn load_settings() -> Settings {
    crate::json_store::load_json(&settings_path(), &settings_migration_chain())
}

pub fn save_settings(s: &Settings) {
    crate::json_store::save_json(&settings_path(), s, false)
}

/// Falls back to a sensible default location rather than blocking first
/// run on a folder picker — matches the existing "sensible default,
/// override in Settings" pattern already used for `scale`/`server_url`.
pub fn resolve_vault_path(settings: &Settings) -> PathBuf {
    if let Some(p) = &settings.vault_path {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    dirs_next::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("prisma-desktop")
        .join("vault")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_vault_path_uses_configured_path_when_set() {
        let mut s = Settings::default();
        s.vault_path = Some("/custom/vault/location".into());
        assert_eq!(resolve_vault_path(&s), PathBuf::from("/custom/vault/location"));
    }

    #[test]
    fn resolve_vault_path_falls_back_when_none() {
        let mut s = Settings::default();
        s.vault_path = None;
        let expected = dirs_next::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("prisma-desktop")
            .join("vault");
        assert_eq!(resolve_vault_path(&s), expected);
    }

    #[test]
    fn resolve_vault_path_falls_back_when_empty_string() {
        let mut s = Settings::default();
        s.vault_path = Some(String::new());
        let expected = dirs_next::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("prisma-desktop")
            .join("vault");
        assert_eq!(resolve_vault_path(&s), expected);
    }

    #[test]
    fn api_url_and_web_url_use_http_when_tls_is_false() {
        let mut s = Settings::default();
        s.hostname = "127.0.0.1".into();
        s.tls = false;
        s.api_port = 8765;
        s.web_port = 8766;
        assert_eq!(s.api_url(), "http://127.0.0.1:8765");
        assert_eq!(s.web_url(), "http://127.0.0.1:8766");
    }

    #[test]
    fn api_url_and_web_url_use_https_when_tls_is_true() {
        let mut s = Settings::default();
        s.hostname = "prisma.example.internal".into();
        s.tls = true;
        s.api_port = 443;
        s.web_port = 443;
        assert_eq!(s.api_url(), "https://prisma.example.internal:443");
        assert_eq!(s.web_url(), "https://prisma.example.internal:443");
    }

    #[test]
    fn an_old_settings_file_with_only_server_url_still_loads_with_defaults() {
        let _guard = crate::json_store::TEST_ENV_GUARD.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}-old-settings", uuid::Uuid::new_v4()));
        std::env::set_var("PRISMA_DESKTOP_CONFIG_DIR", &dir);
        let path = settings_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"scale": 2.0, "server_url": "http://127.0.0.1:9999"}"#).unwrap();

        let loaded = load_settings();

        std::env::remove_var("PRISMA_DESKTOP_CONFIG_DIR");
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(loaded.scale, 2.0);
        assert_eq!(loaded.hostname, "127.0.0.1");
        assert_eq!(loaded.api_port, 8765);
        assert_eq!(loaded.web_port, 8766);
        assert_eq!(loaded.sidebar_width, 220);
    }

    // ── save_settings_cmd: merge, not overwrite ──────────────────────────
    // Regression tests for the actual bug: the frontend's AppSettings never
    // carries window_width/height/x/y/maximized at all, so a full-Settings
    // save_settings_cmd used to silently null them out on every single
    // Settings-page save (Option<T>'s standard serde behavior for a
    // missing key), not just during some narrow timing race.

    fn with_config_dir<R>(f: impl FnOnce() -> R) -> R {
        let _guard = crate::json_store::TEST_ENV_GUARD.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("prisma-desktop-test-{}-cfg", uuid::Uuid::new_v4()));
        std::env::set_var("PRISMA_DESKTOP_CONFIG_DIR", &dir);
        let result = f();
        std::env::remove_var("PRISMA_DESKTOP_CONFIG_DIR");
        std::fs::remove_dir_all(&dir).ok();
        result
    }

    #[test]
    fn save_settings_cmd_preserves_window_geometry_already_on_disk() {
        with_config_dir(|| {
            let mut initial = Settings::default();
            initial.window_width = Some(1200);
            initial.window_height = Some(800);
            initial.window_x = Some(10);
            initial.window_y = Some(20);
            initial.window_maximized = Some(false);
            save_settings(&initial);

            save_settings_cmd(UserSettings {
                scale: 2.0,
                hostname: "example.invalid".into(),
                tls: false,
                api_port: 8765,
                web_port: 8766,
                vault_path: None,
                sidebar_width: 220,
            })
            .unwrap();

            let after = load_settings();
            assert_eq!(after.window_width, Some(1200));
            assert_eq!(after.window_height, Some(800));
            assert_eq!(after.window_x, Some(10));
            assert_eq!(after.window_y, Some(20));
            assert_eq!(after.window_maximized, Some(false));
        });
    }

    #[test]
    fn save_settings_cmd_applies_the_user_fields() {
        with_config_dir(|| {
            save_settings_cmd(UserSettings {
                scale: 3.0,
                hostname: "example.test".into(),
                tls: true,
                api_port: 443,
                web_port: 443,
                vault_path: Some("/custom/vault".into()),
                sidebar_width: 300,
            })
            .unwrap();

            let after = load_settings();
            assert_eq!(after.scale, 3.0);
            assert_eq!(after.hostname, "example.test");
            assert!(after.tls);
            assert_eq!(after.api_port, 443);
            assert_eq!(after.web_port, 443);
            assert_eq!(after.vault_path, Some("/custom/vault".to_string()));
            assert_eq!(after.sidebar_width, 300);
        });
    }
}

#[tauri::command]
pub fn get_settings() -> Settings {
    load_settings()
}

/// The subset of Settings the frontend actually owns and sends on every
/// save (see prisma/ui/src/lib/platform.ts's AppSettings, which this
/// mirrors field-for-field). Deliberately excludes window_width/height/x/y
/// and window_maximized -- those are Rust/window.rs-managed state the
/// frontend never reads or edits. save_settings_cmd used to accept a full
/// Settings instead: Tauri deserializes a JSON payload missing those
/// fields into `None` for each of them (Option<T>'s standard serde
/// behavior for an absent key, no #[serde(default)] needed) -- so *every*
/// save from the Settings page silently wiped the window's remembered
/// size/position/maximized state back to defaults, not just during some
/// narrow race window.
#[derive(Deserialize)]
pub struct UserSettings {
    pub scale: f64,
    pub hostname: String,
    pub tls: bool,
    pub api_port: u16,
    pub web_port: u16,
    pub vault_path: Option<String>,
    pub sidebar_width: u32,
}

#[tauri::command]
pub fn save_settings_cmd(settings: UserSettings) -> Result<(), String> {
    let _lock = SETTINGS_LOCK.lock().unwrap();
    let mut current = load_settings();
    current.scale = settings.scale;
    current.hostname = settings.hostname;
    current.tls = settings.tls;
    current.api_port = settings.api_port;
    current.web_port = settings.web_port;
    current.vault_path = settings.vault_path;
    current.sidebar_width = settings.sidebar_width;
    save_settings(&current);
    Ok(())
}

#[tauri::command]
pub fn pick_vault_folder(app: tauri::AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let folder = app.dialog().file().blocking_pick_folder();
    Ok(folder.map(|p| p.to_string()))
}
