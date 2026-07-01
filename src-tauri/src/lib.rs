use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::Manager;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
struct Settings {
    scale: f64,
    server_url: String,
    window_width: Option<u32>,
    window_height: Option<u32>,
    window_x: Option<i32>,
    window_y: Option<i32>,
    window_maximized: Option<bool>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            scale: 1.0,
            server_url: "http://127.0.0.1:8765".into(),
            window_width: None,
            window_height: None,
            window_x: None,
            window_y: None,
            window_maximized: None,
        }
    }
}

fn settings_path() -> PathBuf {
    let base = dirs_next::config_dir()
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("prisma-desktop").join("settings.json")
}

fn load_settings() -> Settings {
    let path = settings_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_settings(s: &Settings) {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(&path, json);
    }
}

#[tauri::command]
fn get_settings() -> Settings {
    load_settings()
}

#[tauri::command]
fn save_settings_cmd(settings: Settings) -> Result<(), String> {
    save_settings(&settings);
    Ok(())
}

#[tauri::command]
fn apply_scale(window: tauri::WebviewWindow, scale: f64) -> Result<(), String> {
    window.set_zoom(scale).map_err(|e| e.to_string())
}

#[tauri::command]
fn window_start_drag(window: tauri::WebviewWindow) -> Result<(), String> {
    window.start_dragging().map_err(|e| e.to_string())
}

#[tauri::command]
fn window_minimize(window: tauri::WebviewWindow) -> Result<(), String> {
    window.minimize().map_err(|e| e.to_string())
}

#[tauri::command]
fn window_toggle_maximize(window: tauri::WebviewWindow) -> Result<(), String> {
    if window.is_maximized().map_err(|e| e.to_string())? {
        window.unmaximize().map_err(|e| e.to_string())
    } else {
        window.maximize().map_err(|e| e.to_string())
    }
}

#[tauri::command]
fn window_close(window: tauri::WebviewWindow) -> Result<(), String> {
    window.close().map_err(|e| e.to_string())
}

#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    let is_wsl = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|r| r.to_lowercase().contains("microsoft"))
        .unwrap_or(false);
    if is_wsl {
        std::process::Command::new("explorer.exe")
            .arg(&url)
            .spawn()
            .map_err(|e| e.to_string())?;
    } else {
        std::process::Command::new("xdg-open")
            .arg(&url)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let settings = load_settings();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings_cmd,
            apply_scale,
            open_url,
            window_start_drag,
            window_minimize,
            window_toggle_maximize,
            window_close,
        ])
        .setup(move |app| {
            if let Some(win) = app.get_webview_window("main") {
                // bundle.icon in tauri.conf.json only applies to packaged builds —
                // `cargo tauri dev` shows the desktop's generic fallback icon
                // otherwise. Set it explicitly at runtime so dev mode matches too.
                if let Ok(img) = image::load_from_memory(include_bytes!("../icons/128x128.png")) {
                    let rgba = img.to_rgba8();
                    let (width, height) = rgba.dimensions();
                    let icon = tauri::image::Image::new_owned(rgba.into_raw(), width, height);
                    let _ = win.set_icon(icon);
                }

                // Restore window state
                if settings.window_maximized == Some(true) {
                    let _ = win.maximize();
                } else {
                    if let (Some(w), Some(h)) = (settings.window_width, settings.window_height) {
                        let _ = win.set_size(tauri::Size::Physical(tauri::PhysicalSize { width: w, height: h }));
                    }
                    if let (Some(x), Some(y)) = (settings.window_x, settings.window_y) {
                        let _ = win.set_position(tauri::Position::Physical(tauri::PhysicalPosition { x, y }));
                    }
                }

                if settings.scale != 1.0 {
                    let _ = win.set_zoom(settings.scale);
                }

                #[cfg(target_os = "linux")]
                {
                    use webkit2gtk::WebViewExt;
                    let bg = gdk::RGBA::new(0.0, 0.0, 0.0, 0.0);
                    let _ = win.with_webview(move |pv| {
                        pv.inner().set_background_color(&bg);
                    });
                }

                // Debounced window state persistence
                let debounce_gen: Arc<Mutex<u64>> = Arc::new(Mutex::new(0));
                let win_ref = win.clone();
                win.on_window_event(move |event| {
                    let should_save = matches!(
                        event,
                        tauri::WindowEvent::Resized(_) | tauri::WindowEvent::Moved(_)
                    );
                    if !should_save {
                        return;
                    }
                    let gen = {
                        let mut g = debounce_gen.lock().unwrap();
                        *g += 1;
                        *g
                    };
                    let gen_arc = debounce_gen.clone();
                    let win_save = win_ref.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(1000));
                        let current = *gen_arc.lock().unwrap();
                        if current != gen {
                            return;
                        }
                        let mut s = load_settings();
                        let maximized = win_save.is_maximized().unwrap_or(false);
                        s.window_maximized = Some(maximized);
                        if !maximized {
                            if let Ok(size) = win_save.inner_size() {
                                s.window_width = Some(size.width);
                                s.window_height = Some(size.height);
                            }
                            if let Ok(pos) = win_save.outer_position() {
                                s.window_x = Some(pos.x);
                                s.window_y = Some(pos.y);
                            }
                        }
                        save_settings(&s);
                    });
                });
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
