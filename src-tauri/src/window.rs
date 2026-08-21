use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::settings::{load_settings, save_settings, Settings, SETTINGS_LOCK};

const FALLBACK_HTML: &str = include_str!("../fallback.html");

/// The fallback page's own scheme, registered in lib.rs's Builder via
/// `.register_uri_scheme_protocol`. Not file:// -- a file:// page's Origin
/// is `null`/unparseable, which broke `invoke()` at call time ("Origin
/// header is not a valid URL", found live). A registered custom scheme
/// gets a real, valid origin (`fallback://localhost/` on Linux, per
/// tauri::Builder::register_uri_scheme_protocol's own docs), which
/// `invoke()` needs.
pub const FALLBACK_SCHEME: &str = "fallback";

/// Set by resolve_start_url right before it returns the fallback URL for
/// the "server is reachable but the stored session is dead" case (as
/// opposed to "server unreachable at all") -- the registered URI scheme
/// handler in lib.rs renders fallback.html from a plain closure with no
/// request context of its own to carry that distinction through, so it
/// reads this instead. Process-wide by design, same reasoning as
/// SETTINGS_LOCK: exactly one main window, exactly one fallback page live
/// at a time.
pub static FALLBACK_NEEDS_LOGIN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Renders the fallback page's HTML with the current hostname/TLS/ports
/// baked in for display/prefill -- called both by resolve_start_url (to
/// decide the window's target URL) and by the registered protocol handler
/// in lib.rs (to actually serve that content when the webview requests it).
pub fn render_fallback_html(settings: &Settings) -> String {
    let needs_login = FALLBACK_NEEDS_LOGIN.load(std::sync::atomic::Ordering::SeqCst);
    FALLBACK_HTML
        .replace("{{HOSTNAME}}", &settings.hostname)
        .replace("{{TLS_CHECKED_BOOL}}", if settings.tls { "true" } else { "false" })
        .replace("{{API_PORT}}", &settings.api_port.to_string())
        .replace("{{WEB_PORT}}", &settings.web_port.to_string())
        .replace("{{NEEDS_LOGIN_BOOL}}", if needs_login { "true" } else { "false" })
}

/// GET {api_url}/health (prisma's own health route, see prisma/server/app.py)
/// with a short timeout -- an unreachable/down server must not hang app
/// startup waiting on a connection that's never coming.
fn is_server_reachable(api_url: &str) -> bool {
    let Ok(client) = reqwest::Client::builder().timeout(Duration::from_secs(3)).build() else {
        return false;
    };
    let url = format!("{}/health", api_url.trim_end_matches('/'));
    tauri::async_runtime::block_on(async move {
        client.get(&url).send().await.map(|r| r.status().is_success()).unwrap_or(false)
    })
}

fn fallback_url() -> tauri::Url {
    tauri::Url::parse(&format!("{FALLBACK_SCHEME}://localhost/"))
        .expect("scheme://localhost/ is always a valid URL")
}

fn app_url(settings: &Settings) -> Option<tauri::Url> {
    let mut url = tauri::Url::parse(&settings.web_url()).ok()?;
    url.set_path("/app");
    Some(url)
}

/// The URL the main window should load right now: the real server's UI if
/// its API is reachable AND (when a session is stored for it) that session
/// still slides forward via /auth/refresh, the embedded fallback page (with
/// a way to reconfigure and retry, or log in again) otherwise. A server
/// unreachable at all takes priority over the auth check -- no point
/// distinguishing "dead session" from "dead server" when neither lets the
/// app load anyway, and only the former should ever clear a stored session.
pub fn resolve_start_url(settings: &Settings) -> tauri::Url {
    if !is_server_reachable(&settings.api_url()) {
        FALLBACK_NEEDS_LOGIN.store(false, std::sync::atomic::Ordering::SeqCst);
        return fallback_url();
    }
    match crate::auth::check_and_refresh_session(&settings.api_url()) {
        crate::auth::AuthCheckOutcome::Dead => {
            FALLBACK_NEEDS_LOGIN.store(true, std::sync::atomic::Ordering::SeqCst);
            fallback_url()
        }
        crate::auth::AuthCheckOutcome::NoSessionStored
        | crate::auth::AuthCheckOutcome::Refreshed => app_url(settings).unwrap_or_else(fallback_url),
    }
}

/// Called from the fallback page's "Conectar" button (see fallback.html).
/// Saves the new hostname/tls/ports (same merge-preserving path
/// save_settings_cmd uses for window geometry) and, if reachable, navigates
/// the window there directly rather than returning true and making the JS
/// side re-navigate -- one fewer round trip.
///
/// `password` is `Some` when the fallback page is showing its
/// FALLBACK_NEEDS_LOGIN variant (a dead session on an otherwise-reachable
/// server, not a from-scratch reconnect) -- logging in here, before
/// navigating, is what makes that variant actually recover instead of just
/// landing back on /app with no session and repeating the exact failure
/// that routed here in the first place (see resolve_start_url above).
#[tauri::command]
pub async fn try_connect(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    hostname: String,
    tls: bool,
    api_port: u16,
    web_port: u16,
    password: Option<String>,
) -> Result<bool, String> {
    let mut s = load_settings();
    s.hostname = hostname.clone();
    s.tls = tls;
    s.api_port = api_port;
    s.web_port = web_port;

    if !is_server_reachable(&s.api_url()) {
        return Ok(false);
    }

    if let Some(password) = password.filter(|p| !p.is_empty()) {
        crate::auth::sync_login(app, s.api_url(), password).await?;
    }

    {
        let _lock = SETTINGS_LOCK.lock().unwrap();
        save_settings(&s);
    }
    let target = app_url(&s).ok_or_else(|| format!("invalid hostname: {hostname}"))?;
    window.navigate(target).map_err(|e| e.to_string())?;
    Ok(true)
}

#[tauri::command]
pub fn apply_scale(window: tauri::WebviewWindow, scale: f64) -> Result<(), String> {
    window.set_zoom(scale).map_err(|e| e.to_string())
}

/// Linux-only for now -- WSL2 support dropped 2026-07-30 (no Windows/WSL2
/// hardware to test against; a genuinely native Windows build, when that
/// hardware exists, would use a real Windows opener, e.g. reintroducing
/// tauri-plugin-opener, not this xdg-open call).
#[tauri::command]
pub fn open_url(url: String) -> Result<(), String> {
    std::process::Command::new("xdg-open")
        .arg(&url)
        .spawn()
        .map_err(|e| e.to_string())?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_server_reachable_returns_false_for_a_closed_port_without_hanging() {
        // Port 1 is privileged/never listening in any test environment --
        // a real connection-refused case, exercising the same "unreachable"
        // path a genuinely down prisma serve would hit. The 3s client
        // timeout bounds this test's worst case.
        assert!(!is_server_reachable("http://127.0.0.1:1"));
    }

    #[test]
    fn render_fallback_html_embeds_hostname_and_ports() {
        let mut s = Settings::default();
        s.hostname = "example.test".into();
        s.api_port = 1234;
        s.web_port = 5678;
        let html = render_fallback_html(&s);
        assert!(html.contains("example.test"));
        assert!(html.contains("1234"));
        assert!(html.contains("5678"));
        assert!(!html.contains("{{HOSTNAME}}"));
        assert!(!html.contains("{{API_PORT}}"));
        assert!(!html.contains("{{WEB_PORT}}"));
        assert!(!html.contains("{{TLS_CHECKED_BOOL}}"));
    }

    #[test]
    fn resolve_start_url_falls_back_to_the_fallback_scheme_when_server_unreachable() {
        let mut s = Settings::default();
        s.api_port = 1; // never listening, see is_server_reachable's own test
        let url = resolve_start_url(&s);
        assert_eq!(url.scheme(), FALLBACK_SCHEME);
    }

    #[test]
    fn app_url_uses_http_and_the_web_port_when_tls_is_false() {
        let mut s = Settings::default();
        s.hostname = "127.0.0.1".into();
        s.tls = false;
        s.web_port = 8766;
        let url = app_url(&s).unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.port(), Some(8766));
        assert_eq!(url.path(), "/app");
    }

    #[test]
    fn app_url_uses_https_when_tls_is_true() {
        let mut s = Settings::default();
        s.hostname = "prisma.example.internal".into();
        s.tls = true;
        s.web_port = 443;
        let url = app_url(&s).unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("prisma.example.internal"));
        // url crate omits the port from .port() when it's the scheme's
        // default (443 for https) -- port_or_known_default() always returns
        // a value.
        assert_eq!(url.port_or_known_default(), Some(443));
        assert_eq!(url.path(), "/app");
    }
}
