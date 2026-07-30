use std::path::PathBuf;
use std::sync::Mutex;
use serde::{Deserialize, Serialize};

/// Guards every load-modify-save cycle against settings.json -- both
/// save_settings_cmd (below) and window.rs's debounced resize/move handler
/// do "load current, mutate a subset of fields, write the whole file back."
/// Without this, the two could interleave: e.g. a resize's load happening
/// between save_settings_cmd's load and its write would have the resize's
/// save undo whatever save_settings_cmd just persisted (or vice versa).
/// A single process-wide lock is simple and sufficient here -- these are
/// both low-frequency, user-triggered-or-debounced writes, not a hot path.
pub(crate) static SETTINGS_LOCK: Mutex<()> = Mutex::new(());

#[derive(Serialize, Deserialize, Clone)]
pub struct SavedServer {
    pub name: String,
    pub url: String,
}

fn default_saved_servers() -> Vec<SavedServer> {
    vec![
        SavedServer { name: "Local".into(), url: "http://127.0.0.1:8765".into() },
        // Generic label — "Forge" is this maintainer's own private server
        // name, not a sensible default for other users of this software.
        SavedServer { name: "Remote".into(), url: "https://prisma.forge.internal".into() },
    ]
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Settings {
    pub scale: f64,
    pub server_url: String,
    pub window_width: Option<u32>,
    pub window_height: Option<u32>,
    pub window_x: Option<i32>,
    pub window_y: Option<i32>,
    pub window_maximized: Option<bool>,
    // Where the local vault .md copy lives. None until the user picks a
    // folder (or a first sync run falls back to a sensible default) — see
    // sync::manifest for how an unset path is handled at startup.
    pub vault_path: Option<String>,
    // Named shortcuts shown as a one-click switcher in the Settings page
    // (see +page.svelte) — `server_url` above is still the actual active
    // value; this is just a quick way to set it without retyping.
    // `#[serde(default...)]` so an existing settings.json from before this
    // field existed still deserializes instead of failing outright.
    #[serde(default = "default_saved_servers")]
    pub saved_servers: Vec<SavedServer>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // 1x reads as uncomfortably small on today's typical high-density
            // displays (confirmed live on a 4K monitor) — 1.5x is a more
            // usable out-of-the-box default for a fresh install with no
            // settings.json yet; still adjustable 1x-5x in Settings.
            scale: 1.5,
            server_url: "http://127.0.0.1:8765".into(),
            window_width: None,
            window_height: None,
            window_x: None,
            window_y: None,
            window_maximized: None,
            vault_path: None,
            saved_servers: default_saved_servers(),
        }
    }
}

pub fn settings_path() -> PathBuf {
    crate::json_store::config_file_path("settings.json")
}

pub fn load_settings() -> Settings {
    crate::json_store::load_json(&settings_path())
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
                server_url: "http://example.invalid".into(),
                saved_servers: vec![],
                vault_path: None,
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
                server_url: "https://example.test".into(),
                saved_servers: vec![SavedServer { name: "X".into(), url: "https://x.test".into() }],
                vault_path: Some("/custom/vault".into()),
            })
            .unwrap();

            let after = load_settings();
            assert_eq!(after.scale, 3.0);
            assert_eq!(after.server_url, "https://example.test");
            assert_eq!(after.saved_servers.len(), 1);
            assert_eq!(after.saved_servers[0].name, "X");
            assert_eq!(after.vault_path, Some("/custom/vault".to_string()));
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
    pub server_url: String,
    pub saved_servers: Vec<SavedServer>,
    pub vault_path: Option<String>,
}

#[tauri::command]
pub fn save_settings_cmd(settings: UserSettings) -> Result<(), String> {
    let _lock = SETTINGS_LOCK.lock().unwrap();
    let mut current = load_settings();
    current.scale = settings.scale;
    current.server_url = settings.server_url;
    current.saved_servers = settings.saved_servers;
    current.vault_path = settings.vault_path;
    save_settings(&current);
    Ok(())
}

#[tauri::command]
pub fn pick_vault_folder(app: tauri::AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let folder = app.dialog().file().blocking_pick_folder();
    Ok(folder.map(|p| p.to_string()))
}
