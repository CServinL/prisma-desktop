mod json_store;
mod settings;
mod window;
mod auth;
mod sync;

use tauri::Manager;

use settings::{get_settings, load_settings, pick_vault_folder, save_settings_cmd};
use window::{
    apply_scale, init_main_window, open_url, window_close, window_minimize,
    window_start_drag, window_toggle_maximize,
};
use auth::{sync_login, sync_logout, sync_status};
use sync::{sync_diff, sync_engine_status, sync_start, sync_stop, AppState};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let settings = load_settings();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings_cmd,
            pick_vault_folder,
            apply_scale,
            open_url,
            window_start_drag,
            window_minimize,
            window_toggle_maximize,
            window_close,
            sync_login,
            sync_logout,
            sync_status,
            sync_start,
            sync_stop,
            sync_engine_status,
            sync_diff,
        ])
        .setup(move |app| {
            if let Some(win) = app.get_webview_window("main") {
                init_main_window(&win, &settings);
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
