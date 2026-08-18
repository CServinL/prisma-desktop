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
                // Same path in dev and production: settings.json is the
                // only source of truth for which server this window talks
                // to. devUrl (tauri.conf.json) is just the placeholder
                // `cargo tauri dev` shows for an instant before this
                // navigate() replaces it -- its value no longer matters.
                // (Previously gated behind `!tauri::is_dev()`, working
                // around an app_url() bug that used the wrong port; fixed
                // together with this gate's original commit, but the gate
                // itself was never removed afterward.)
                let start_url = resolve_start_url(&settings);
                let _ = win.navigate(start_url);
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
