mod json_store;
mod schema_gov;
mod settings;
mod window;
mod auth;
mod sync;

use tauri::Manager;

use settings::{get_settings, load_settings, pick_vault_folder, save_settings_cmd};
use window::{
    apply_scale, init_main_window, open_url, render_fallback_html, resolve_start_url, try_connect,
    FALLBACK_SCHEME,
};
use auth::{sync_login, sync_logout, sync_status};
use sync::{sync_diff, sync_engine_status, sync_start, sync_stop, AppState};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let settings = load_settings();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .register_uri_scheme_protocol(FALLBACK_SCHEME, |_ctx, _request| {
            let html = render_fallback_html(&load_settings());
            tauri::http::Response::builder()
                .header("Content-Type", "text/html")
                .body(html.into_bytes())
                .unwrap()
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings_cmd,
            pick_vault_folder,
            apply_scale,
            open_url,
            sync_login,
            sync_logout,
            sync_status,
            sync_start,
            sync_stop,
            sync_engine_status,
            sync_diff,
            try_connect,
        ])
        .setup(move |app| {
            if let Some(win) = app.get_webview_window("main") {
                init_main_window(&win, &settings);
                // `tauri dev` already loads devUrl correctly on its own --
                // navigating here too was clobbering it with
                // {settings.server_url}/app regardless of devUrl, breaking
                // `cargo tauri dev` (found live: hit the API port's /app,
                // a real 404, instead of the web port devUrl already had
                // loaded). Only override for an actual installed/run binary.
                if !tauri::is_dev() {
                    let start_url = resolve_start_url(&settings);
                    let _ = win.navigate(start_url);
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
