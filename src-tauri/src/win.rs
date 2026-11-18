//! Window creation. Every window is created hidden with a themed background and is
//! shown only after its page has rendered its payload (see shim.js) — no blank frame.

use crate::St;
use serde_json::Value;
use std::{thread, time::Duration};
use tauri::{
    window::Color, AppHandle, Manager, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};

const SHIM: &str = include_str!("shim.js");

/// Fullscreen overlays that must hold focus and can't be closed by the user.
pub fn is_overlay(label: &str) -> bool {
    matches!(label, "break" | "sleep-warning" | "shutdown-prompt" | "game-blocked")
}

// Mirrors --overlay-bg-1 / --bg-base in themes.css.
fn theme_bg(theme: &str, dark: bool) -> Color {
    let (dark_bg, light_bg): (u32, u32) = match theme {
        "ocean" => (0x0d1520, 0xf5f8fa),
        "forest" => (0x0d150d, 0xf5f8f5),
        "lavender" => (0x150d18, 0xf8f5fa),
        "slate" => (0x12151a, 0xf5f6f7),
        "rose" => (0x180d10, 0xfaf5f6),
        _ => (0x1a1612, 0xfaf8f5),
    };
    let c = if dark { dark_bg } else { light_bg };
    Color((c >> 16) as u8, (c >> 8) as u8, c as u8, 255)
}

fn builder<'a>(
    app: &'a AppHandle,
    st: &mut St,
    label: &str,
    payload: Value,
) -> WebviewWindowBuilder<'a, tauri::Wry, AppHandle> {
    st.payloads.insert(label.into(), payload);
    if crate::data::is_test_dir() {
        eprintln!("{label} create {}", crate::mono());
    }
    WebviewWindowBuilder::new(app, label, WebviewUrl::App(format!("{label}.html").into()))
        .initialization_script(SHIM)
        .visible(false)
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
}

fn finish(app: &AppHandle, label: &str, built: tauri::Result<WebviewWindow>) -> Option<WebviewWindow> {
    let window = built.map_err(|err| eprintln!("window {label} failed: {err}")).ok()?;
    safety_net(app, label);
    Some(window)
}

// An overlay that never appears is worse than one that flashes.
fn safety_net(app: &AppHandle, label: &str) {
    let (app, label) = (app.clone(), label.to_string());
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(1500));
        crate::with(&app, |st| reveal(&app, st, &label));
    });
}

pub fn is_panel(label: &str) -> bool {
    matches!(label, "settings" | "stats")
}

/// Fullscreen always-on-top overlay. Returns false if it already exists or failed.
pub fn overlay(app: &AppHandle, st: &mut St, label: &str, payload: Value) -> bool {
    if app.get_webview_window(label).is_some() {
        return false;
    }
    let bg = theme_bg(&st.settings.theme, true);
    let built = builder(app, st, label, payload)
        .title("StandBuddy")
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .closable(false)
        .fullscreen(true)
        .background_color(bg)
        .build();
    finish(app, label, built).is_some()
}

/// Frameless panel window (Settings, Stats). Panels are created hidden when the cursor
/// reaches the tray icon (`prewarm`), so the click only re-renders fresh data and shows
/// the window. Focuses the window if it is already showing.
pub fn panel(app: &AppHandle, st: &mut St, label: &str, title: &str, w: f64, h: f64, payload: Value) {
    st.wanted.insert(label.into());
    if crate::data::is_test_dir() {
        eprintln!("{label} open {}", crate::mono());
    }
    if let Some(existing) = app.get_webview_window(label) {
        if existing.is_visible().unwrap_or(false) {
            let _ = existing.set_focus();
            return;
        }
        st.payloads.insert(label.into(), payload);
        let _ = existing.set_background_color(Some(theme_bg(&st.settings.theme, st.settings.dark)));
        // If the page hasn't loaded yet, its own first load reads this payload and reveals.
        let _ = existing.eval("window.__sbRefresh && window.__sbRefresh()");
        safety_net(app, label);
        return;
    }
    create_panel(app, st, label, title, w, h, payload);
}

/// Create a panel hidden so the first `panel` call is instant. No-op if it exists.
pub fn prewarm(app: &AppHandle, st: &mut St, label: &str, title: &str, w: f64, h: f64, payload: Value) {
    if app.get_webview_window(label).is_none() {
        create_panel(app, st, label, title, w, h, payload);
    }
}

/// Close a panel: hide it and reload the page in the background, so a reopen within
/// `WARM_MS` is instant and starts clean (no half-edited form or scroll position).
/// After that the tick destroys it.
pub fn hide_panel(app: &AppHandle, st: &mut St, label: &str) {
    st.wanted.remove(label);
    st.warm_until = crate::mono() + crate::WARM_MS;
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.hide();
        let _ = window.eval("location.reload()");
    }
}

fn create_panel(app: &AppHandle, st: &mut St, label: &str, title: &str, w: f64, h: f64, payload: Value) {
    let bg = theme_bg(&st.settings.theme, st.settings.dark);
    // No native title bar: the page draws its own header (drag + close) in the theme.
    let built = builder(app, st, label, payload)
        .title(title)
        .inner_size(w, h)
        .decorations(false)
        .shadow(true)
        .background_color(bg)
        .build();
    finish(app, label, built);
}

/// Click-through toast in the bottom-right corner; never takes focus from the game.
pub fn toast(app: &AppHandle, st: &mut St, label: &str, payload: Value) {
    if app.get_webview_window(label).is_some() {
        return;
    }
    const W: f64 = 380.0;
    const H: f64 = 150.0;
    let (x, y) = match app.primary_monitor() {
        Ok(Some(m)) => {
            let (area, scale) = (m.work_area(), m.scale_factor()); // work area excludes the taskbar
            (
                (area.position.x as f64 + area.size.width as f64) / scale - W,
                (area.position.y as f64 + area.size.height as f64) / scale - H,
            )
        }
        _ => (0.0, 0.0),
    };
    let built = builder(app, st, label, payload)
        .title("StandBuddy")
        .inner_size(W, H)
        .position(x, y)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .focusable(false)
        .build();
    if let Some(window) = finish(app, label, built) {
        let _ = window.set_ignore_cursor_events(true);
    }
}

/// Show a window once its content is painted. Idempotent.
pub fn reveal(app: &AppHandle, st: &mut St, label: &str) {
    let Some(window) = app.get_webview_window(label) else { return };
    if window.is_visible().unwrap_or(true) {
        return;
    }
    // A prewarmed or reloading panel nobody asked to open stays hidden.
    if is_panel(label) && !st.wanted.contains(label) {
        return;
    }
    let _ = window.show();
    if crate::data::is_test_dir() {
        eprintln!("{label} reveal {}", crate::mono());
    }
    if is_overlay(label) || is_panel(label) {
        let _ = window.set_focus();
    }
    if label == "break" && st.break_started.is_none() {
        st.record_break_shown();
    }
}

pub fn destroy(app: &AppHandle, st: &mut St, label: &str) {
    st.payloads.remove(label);
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.destroy();
    }
}
