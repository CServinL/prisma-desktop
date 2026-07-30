use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::settings::{load_settings, save_settings, SETTINGS_LOCK};

#[tauri::command]
pub fn apply_scale(window: tauri::WebviewWindow, scale: f64) -> Result<(), String> {
    window.set_zoom(scale).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn window_start_drag(window: tauri::WebviewWindow) -> Result<(), String> {
    window.start_dragging().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn window_minimize(window: tauri::WebviewWindow) -> Result<(), String> {
    window.minimize().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn window_toggle_maximize(window: tauri::WebviewWindow) -> Result<(), String> {
    if window.is_maximized().map_err(|e| e.to_string())? {
        window.unmaximize().map_err(|e| e.to_string())
    } else {
        window.maximize().map_err(|e| e.to_string())
    }
}

#[tauri::command]
pub fn window_close(window: tauri::WebviewWindow) -> Result<(), String> {
    window.close().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn open_url(url: String) -> Result<(), String> {
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

/// Restores window geometry/scale/icon from settings and installs the
/// debounced resize/move listener that persists them back. Called once
/// from `run()`'s `.setup()` closure for the "main" window.
pub fn init_main_window(win: &tauri::WebviewWindow, settings: &crate::settings::Settings) {
    // bundle.icon in tauri.conf.json only applies to packaged builds —
    // `cargo tauri dev` shows the desktop's generic fallback icon
    // otherwise. Set it explicitly at runtime so dev mode matches too.
    if let Ok(img) = image::load_from_memory(include_bytes!("../icons/128x128.png")) {
        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        let icon = tauri::image::Image::new_owned(rgba.into_raw(), width, height);
        let _ = win.set_icon(icon);
    }

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
            // Held across the whole load-modify-save cycle -- see
            // SETTINGS_LOCK's doc comment (settings.rs) for why a bare
            // load_settings() here without it could race save_settings_cmd
            // and silently lose whichever side's fields lost the race.
            let _lock = SETTINGS_LOCK.lock().unwrap();
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
