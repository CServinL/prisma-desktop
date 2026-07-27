use std::path::PathBuf;
use serde::{Deserialize, Serialize};

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
}

#[tauri::command]
pub fn get_settings() -> Settings {
    load_settings()
}

#[tauri::command]
pub fn save_settings_cmd(settings: Settings) -> Result<(), String> {
    save_settings(&settings);
    Ok(())
}

#[tauri::command]
pub fn pick_vault_folder(app: tauri::AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let folder = app.dialog().file().blocking_pick_folder();
    Ok(folder.map(|p| p.to_string()))
}
