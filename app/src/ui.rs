//! Window layer: the tray and the Slint windows. Windows not ported yet (Phase 3, one per
//! step) are stubs that only track what is open and log it in test mode.

use crate::{bg, data, emoji, mono, St};
use serde_json::Value;
use slint::{winit_030::WinitWindowAccessor, CloseRequestResponse, ComponentHandle, Image, ModelRc, Timer, TimerMode, VecModel};
use std::{any::Any, cell::Cell, cell::RefCell, collections::HashMap, collections::HashSet, time::Duration};
use windows_sys::Win32::{
    Foundation::{BOOL, HWND, LPARAM, POINT, RECT},
    Graphics::Gdi::{GetMonitorInfoW, InvalidateRect, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST},
    System::Threading::GetCurrentThreadId,
    UI::{
        HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
        WindowsAndMessaging::{
            EnumThreadWindows, GetCursorPos, GetForegroundWindow, GetWindowLongPtrW, GetWindowTextW, IsWindowVisible,
            SetForegroundWindow, SetLayeredWindowAttributes, SetWindowLongPtrW, GWL_EXSTYLE, LWA_COLORKEY,
            WS_EX_APPWINDOW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
        },
    },
};

slint::include_modules!();

/// A live Slint window: the component (kept alive), how to hide it, and its timers.
struct Live {
    _component: Box<dyn Any>,
    hide: Box<dyn Fn()>,
    timers: Vec<Timer>,
}

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
    static OPEN: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static LIVE: RefCell<HashMap<String, Live>> = RefCell::new(HashMap::new());
    static BREAK: RefCell<Option<slint::Weak<Break>>> = const { RefCell::new(None) };
    static ICONS: RefCell<HashMap<(String, u32, bool), Image>> = RefCell::new(HashMap::new());
    /// Read by the winit attributes hook while a window is created (`Component::new`).
    static CREATE: Cell<Create> = const { Cell::new(Create::Normal) };
}

/// How the winit window of the next component is created.
#[derive(Clone, Copy)]
enum Create {
    Normal,
    /// Shown without activation (the toast must not take focus from a game).
    NoActivate,
    /// Frameless panel with the system shadow and rounded corners (v2: Tauri `shadow(true)`).
    Panel,
}

/// Create a component with `mode` applied to its native window.
fn create<T>(mode: Create, new: impl FnOnce() -> Result<T, slint::PlatformError>) -> Result<T, slint::PlatformError> {
    CREATE.with(|c| c.set(mode));
    let made = new();
    CREATE.with(|c| c.set(Create::Normal));
    made
}

/// Fullscreen overlays that must hold focus and can't be closed by the user.
pub fn is_overlay(label: &str) -> bool {
    matches!(label, "break" | "sleep-warning" | "shutdown-prompt" | "game-blocked")
}

fn log(label: &str, what: &str) {
    if data::is_test_dir() {
        eprintln!("{label} {what} {}", mono());
    }
}

/// Opens `label` once; false if it is already open.
fn open(label: &str) -> bool {
    let added = OPEN.with(|open| open.borrow_mut().insert(label.to_string()));
    if added {
        log(label, "show");
    }
    added
}

// --- Win32 helpers for overlays ----------------------------------------------------

/// The top-level window of this thread titled `title` (Slint doesn't expose the HWND
/// without another crate; the title is unique per window kind).
fn thread_window(title: &str) -> Option<HWND> {
    struct Find { want: Vec<u16>, found: HWND }
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let find = &mut *(lparam as *mut Find);
        let mut buf = [0u16; 64];
        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32).max(0) as usize;
        if buf[..n] == find.want[..] {
            find.found = hwnd;
            return 0;
        }
        1
    }
    let mut find = Find { want: title.encode_utf16().collect(), found: std::ptr::null_mut() };
    unsafe { EnumThreadWindows(GetCurrentThreadId(), Some(each), &mut find as *mut Find as LPARAM) };
    (!find.found.is_null()).then_some(find.found)
}

/// The monitor under the cursor: full rect, work area (without the taskbar), scale factor.
fn cursor_monitor() -> (RECT, RECT, f32) {
    unsafe {
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        let monitor = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(monitor, &mut mi);
        let (mut dpi, mut dpi_y) = (96u32, 96u32);
        GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y);
        (mi.rcMonitor, mi.rcWork, dpi.max(96) as f32 / 96.0)
    }
}

/// Top-left corner for a `w`×`h` window next to `pt`, kept `margin` inside `work`. The
/// panels open from the tray menu, so the cursor sits by the tray: clamping into the
/// work area puts the window against the taskbar, beside the tray, on whichever edge it is.
fn near_point(pt: POINT, (w, h): (i32, i32), work: RECT, margin: i32) -> (i32, i32) {
    let fit = |v: i32, lo: i32, hi: i32| if hi < lo { lo } else { v.clamp(lo, hi) };
    (
        fit(pt.x - w / 2, work.left + margin, work.right - w - margin),
        fit(pt.y - h / 2, work.top + margin, work.bottom - h - margin),
    )
}

/// Move a shown-but-unmapped panel next to the cursor (the tray).
fn place_near_cursor(window: &slint::Window) {
    let (_, work, scale) = cursor_monitor();
    let mut pt = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut pt) };
    let size = window.size();
    let (x, y) = near_point(pt, (size.width as i32, size.height as i32), work, (12.0 * scale) as i32);
    window.set_position(slint::PhysicalPosition::new(x, y));
}

/// Borderless, always-on-top window covering the monitor under the cursor — no
/// fullscreen mode switch (~20 ms cheaper, specs/native-ui.md Phase 0).
fn cover_cursor_monitor(window: &slint::Window) -> f32 {
    let (r, _, _) = cursor_monitor();
    let (w, h) = ((r.right - r.left).max(1), (r.bottom - r.top).max(1));
    window.set_position(slint::PhysicalPosition::new(r.left, r.top));
    window.set_size(slint::PhysicalSize::new(w as u32, h as u32));
    w as f32 / h as f32
}

/// After `show()`: once the native window exists, drop it from the taskbar and take
/// focus; once it is mapped (Slint draws the first frame before mapping), report
/// `on_shown`. Then keep focus every 500 ms while it is up.
fn overlay_timers(label: &'static str, title: &'static str) -> Vec<Timer> {
    let probe = Timer::default();
    let (mut styled, mut done) = (false, false);
    let started = mono();
    probe.start(TimerMode::Repeated, Duration::from_millis(2), move || {
        let Some(hwnd) = thread_window(title) else { return };
        unsafe {
            if !styled {
                styled = true;
                let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (ex | WS_EX_TOOLWINDOW as isize) & !(WS_EX_APPWINDOW as isize));
                SetForegroundWindow(hwnd);
            }
            if !done && IsWindowVisible(hwnd) != 0 {
                done = true;
                repaint(hwnd);
                log(label, &format!("painted +{}ms", mono() - started));
                stop_probe(label);
                Timer::single_shot(Duration::ZERO, move || crate::with(|st| crate::on_shown(st, label)));
            }
        }
    });
    let focus = Timer::default();
    focus.start(TimerMode::Repeated, Duration::from_millis(500), move || {
        if let Some(hwnd) = thread_window(title) {
            unsafe {
                if GetForegroundWindow() != hwnd {
                    SetForegroundWindow(hwnd);
                }
            }
        }
    });
    vec![probe, focus]
}

/// Slint draws the first frame before the window is mapped, and Windows discards what is
/// drawn into an unmapped window; a window with no animation would stay blank (white)
/// until something else repainted it. Ask for a full repaint once it is mapped.
fn repaint(hwnd: HWND) {
    unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
}

/// Drop a window's first timer (its probe) from outside the probe's own callback.
fn stop_probe(label: &'static str) {
    Timer::single_shot(Duration::ZERO, move || {
        LIVE.with(|live| live.borrow_mut().get_mut(label).map(|l| l.timers.remove(0)));
    });
}

// --- Overlays -----------------------------------------------------------------------

/// A v2 emoji at CSS `font-size: css_px`, as a color (or grayscale) image drawn once at 2×.
/// `emoji::render` fits the glyph at 0.78 of its square, so the square (and the Slint
/// image box) is css_px / 0.78: 24 → 31, 34 → 44, 72 → 92, 88 → 113 px.
fn icon_with(emoji: &str, css_px: u32, gray: bool) -> Image {
    let side = (css_px as f32 / 0.78).round() as u32;
    ICONS.with(|icons| {
        let key = (emoji.to_string(), css_px, gray);
        icons.borrow_mut().entry(key).or_insert_with(|| emoji::render(emoji, side * 2, gray).unwrap_or_default()).clone()
    })
}

fn icon(emoji: &str, css_px: u32) -> Image {
    icon_with(emoji, css_px, false)
}

const BREAK_ICON: (&str, u32) = ("\u{1F938}", 72); // 🤸, break.html
const MOON_ICON: (&str, u32) = ("\u{1F319}", 88); // 🌙, sleep-warning.html
const SLEEP_ICON: (&str, u32) = ("\u{1F634}", 88); // 😴, shutdown-prompt.html
const HOURGLASS_ICON: (&str, u32) = ("\u{23F3}", 88); // ⏳, game-blocked.html
const GAME_ICON: (&str, u32) = ("\u{1F3AE}", 34); // 🎮, game-warning.html
const BADGE_PX: u32 = 24; // stats.html .achievement-icon

/// Draw the overlay icons ahead of time: the first DirectWrite/Direct2D call costs ~60 ms,
/// which would otherwise land between the trigger and the overlay's first frame.
pub fn warm_up() {
    icon(BREAK_ICON.0, BREAK_ICON.1);
    for (emoji, px) in [MOON_ICON, SLEEP_ICON, HOURGLASS_ICON, GAME_ICON] {
        icon(emoji, px);
    }
    for achievement in data::ACHIEVEMENTS {
        icon_with(achievement.3, BADGE_PX, false);
        icon_with(achievement.3, BADGE_PX, true);
    }
}

/// Select Slint's winit backend with the software renderer, with a hook that creates a
/// window inactive while `NO_ACTIVATE` is set (the toast must not take focus from a game).
pub fn select_backend() -> Result<(), slint::PlatformError> {
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .with_winit_window_attributes_hook(|attrs| {
            use slint::winit_030::winit::platform::windows::WindowAttributesExtWindows;
            match CREATE.with(Cell::get) {
                Create::Normal => attrs,
                Create::NoActivate => attrs.with_active(false),
                Create::Panel => attrs.with_undecorated_shadow(true),
            }
        })
        .select()
}

fn palette_of(payload: &Value) -> i32 {
    bg::palette_index(payload["theme"].as_str().unwrap_or("warm"))
}

/// Every overlay: block Alt+F4, show it, keep it alive with its probe + focus timers.
fn present<C: ComponentHandle + 'static>(win: C, label: &'static str, title: &'static str) -> Result<Live, slint::PlatformError> {
    win.window().on_close_requested(|| CloseRequestResponse::KeepWindowShown);
    win.show()?;
    let weak = win.as_weak();
    Ok(Live {
        hide: Box::new(move || if let Some(w) = weak.upgrade() { let _ = w.hide(); }),
        _component: Box::new(win),
        timers: overlay_timers(label, title),
    })
}

/// A window callback that closes its own window: run it after the callback returns.
fn later(action: impl FnOnce(&mut St) + 'static) {
    Timer::single_shot(Duration::ZERO, move || crate::with(action));
}

fn format_left(ms: i64) -> String {
    let secs = (ms.max(0) + 999) / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn open_break(payload: &Value) -> Result<Live, slint::PlatformError> {
    let win = Break::new()?;
    let palette = palette_of(payload);
    win.global::<Theme>().set_palette(palette);
    win.set_break_icon(icon(BREAK_ICON.0, BREAK_ICON.1));
    win.set_time(format_left((payload["duration"].as_f64().unwrap_or(60.0) * 1000.0) as i64).into());
    win.set_emergency(payload["emergencyUnlock"].as_bool() != Some(false));
    win.set_tip_text(payload["tip"]["text"].as_str().unwrap_or("").into());
    win.set_tip_category(payload["tip"]["category"].as_str().unwrap_or("").to_uppercase().into());
    win.on_skip(|| later(crate::break_skip));
    let aspect = cover_cursor_monitor(win.window());
    win.set_bg(bg::overlay_background(palette, aspect, bg::BREAK));
    BREAK.with(|b| *b.borrow_mut() = Some(win.as_weak()));
    present(win, "break", "StandBuddy break")
}

fn open_sleep_warning(payload: &Value) -> Result<Live, slint::PlatformError> {
    let win = SleepWarning::new()?;
    let palette = palette_of(payload);
    win.global::<Theme>().set_palette(palette);
    win.set_moon_icon(icon(MOON_ICON.0, MOON_ICON.1));
    win.set_minutes_left(payload["minutesLeft"].as_i64().unwrap_or(30) as i32);
    win.on_done(|| later(crate::sleep_warning_close));
    let aspect = cover_cursor_monitor(win.window());
    win.set_bg(bg::overlay_background(palette, aspect, bg::MOON));
    present(win, "sleep-warning", "StandBuddy bedtime warning")
}

fn open_shutdown_prompt(payload: &Value) -> Result<Live, slint::PlatformError> {
    let win = ShutdownPrompt::new()?;
    let palette = palette_of(payload);
    win.global::<Theme>().set_palette(palette);
    win.set_sleep_icon(icon(SLEEP_ICON.0, SLEEP_ICON.1));
    win.set_snoozes_left(payload["snoozesLeft"].as_i64().unwrap_or(0) as i32);
    win.on_snooze(|left| later(move |st| crate::shutdown_snooze(st, left.max(0) as u32)));
    win.on_shutdown(|| later(crate::shutdown_execute));
    let aspect = cover_cursor_monitor(win.window());
    win.set_bg(bg::overlay_background(palette, aspect, bg::SHUTDOWN));
    present(win, "shutdown-prompt", "StandBuddy shutdown prompt")
}

fn open_game_blocked(payload: &Value) -> Result<Live, slint::PlatformError> {
    let win = GameBlocked::new()?;
    let palette = palette_of(payload);
    win.global::<Theme>().set_palette(palette);
    win.set_hourglass_icon(icon(HOURGLASS_ICON.0, HOURGLASS_ICON.1));
    let hours = payload["limitHours"].as_f64().filter(|h| *h > 0.0);
    win.set_limit_hours(hours.map(|h| format!("{h}")).unwrap_or_default().into());
    let aspect = cover_cursor_monitor(win.window());
    win.set_bg(bg::overlay_background(palette, aspect, bg::MOON));
    present(win, "game-blocked", "StandBuddy game limit")
}

/// The game-warning toast: bottom-right of the cursor's work area, never activated,
/// click-through, transparent outside the toast via a colour key; plays the chime.
fn open_toast(payload: &Value) -> Result<Live, slint::PlatformError> {
    let win = create(Create::NoActivate, GameWarning::new)?;
    let palette = palette_of(payload);
    win.global::<Theme>().set_palette(palette);
    let key = bg::key_color(palette);
    win.set_key(slint::Color::from_rgb_u8((key >> 16) as u8, (key >> 8) as u8, key as u8));
    win.set_game_icon(icon(GAME_ICON.0, GAME_ICON.1));
    win.set_minutes_left(payload["minutesLeft"].as_i64().unwrap_or(30) as i32);
    let (_, work, scale) = cursor_monitor();
    let (w, h) = (win.get_outer_width() * scale, win.get_outer_height() * scale);
    let margin = 16.0 * scale;
    win.window().set_position(slint::PhysicalPosition::new(
        (work.right as f32 - margin - w).round() as i32,
        (work.bottom as f32 - margin - h).round() as i32,
    ));
    win.show()?;
    crate::chime::play();
    // COLORREF is 0x00BBGGRR.
    let colorref = (key >> 16 & 255) | (key & 0xff00) | (key & 255) << 16;
    let probe = Timer::default();
    let mut done = false;
    probe.start(TimerMode::Repeated, Duration::from_millis(2), move || {
        let Some(hwnd) = thread_window("StandBuddy game warning").filter(|_| !done) else { return };
        done = true;
        unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let add = (WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW) as isize;
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (ex | add) & !(WS_EX_APPWINDOW as isize));
            SetLayeredWindowAttributes(hwnd, colorref, 0, LWA_COLORKEY);
        }
        stop_probe("game-warning");
    });
    let weak = win.as_weak();
    Ok(Live {
        hide: Box::new(move || if let Some(w) = weak.upgrade() { let _ = w.hide(); }),
        _component: Box::new(win),
        timers: vec![probe],
    })
}

// --- Panels ---------------------------------------------------------------------------

/// Theme of a panel from its payload: settings.json's `theme` and `dark` (dark unless false).
fn panel_theme(theme: &Theme, payload: &Value) {
    theme.set_palette(palette_of(payload));
    theme.set_dark(payload["dark"].as_bool() != Some(false));
}

/// Every panel: close on Alt+F4 / close button via `destroy`, show, take focus once painted.
fn present_panel<C: ComponentHandle + 'static>(win: C, label: &'static str, title: &'static str) -> Result<Live, slint::PlatformError> {
    win.window().on_close_requested(move || {
        later(move |st| destroy(st, label));
        CloseRequestResponse::KeepWindowShown
    });
    win.show()?;
    place_near_cursor(win.window());
    let probe = Timer::default();
    let (mut done, started) = (false, mono());
    probe.start(TimerMode::Repeated, Duration::from_millis(2), move || {
        let Some(hwnd) = thread_window(title).filter(|_| !done) else { return };
        if unsafe { IsWindowVisible(hwnd) } == 0 {
            return;
        }
        done = true;
        repaint(hwnd);
        unsafe { SetForegroundWindow(hwnd) };
        log(label, &format!("painted +{}ms", mono() - started));
        stop_probe(label);
    });
    let weak = win.as_weak();
    Ok(Live {
        hide: Box::new(move || if let Some(w) = weak.upgrade() { let _ = w.hide(); }),
        _component: Box::new(win),
        timers: vec![probe],
    })
}

/// Header drag = native title-bar drag.
fn drag(window: &slint::Window) {
    window.with_winit_window(|w| w.drag_window());
}

/// "12.5" or "3" like v2's `Math.round(x * 10) / 10`.
fn one_decimal(x: f64) -> String {
    let r = (x * 10.0).round() / 10.0;
    if r.fract() == 0.0 { format!("{}", r as i64) } else { format!("{r:.1}") }
}

fn fill_stats(win: &Stats, data: &Value) {
    let today = &data["today"];
    let int = |v: &Value| v.as_i64().unwrap_or(0) as i32;
    win.set_streak(int(&data["streak"]));
    win.set_today_completed(int(&today["completed"]));
    win.set_today_emergency(int(&today["emergency"]));
    win.set_today_shown(int(&today["shown"]));
    win.set_today_minutes(one_decimal(today["breakMinutes"].as_f64().unwrap_or(0.0)).into());
    let days: Vec<StatsDay> = data["last7Days"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|d| StatsDay { completed: int(&d["completed"]), name: d["dayName"].as_str().unwrap_or("").into() })
        .collect();
    win.set_max_completed(days.iter().map(|d| d.completed).max().unwrap_or(0).max(1));
    win.set_days(ModelRc::new(VecModel::from(days)));
    let avg = data["avgMinutesBetween"].as_f64().unwrap_or(0.0);
    win.set_avg_text(if avg > 0.0 {
        format!("Completed breaks per day. Reminder interval: {} min", one_decimal(avg)).into()
    } else {
        "No completed breaks this week yet.".into()
    });
    let badges: Vec<Badge> = data["achievements"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| {
            let unlocked = a["unlocked"].as_bool().unwrap_or(false);
            Badge {
                name: a["name"].as_str().unwrap_or("").into(),
                description: a["description"].as_str().unwrap_or("").into(),
                icon: icon_with(a["icon"].as_str().unwrap_or(""), BADGE_PX, !unlocked),
                unlocked,
            }
        })
        .collect();
    win.set_unlocked_count(badges.iter().filter(|b| b.unlocked).count() as i32);
    win.set_badges(ModelRc::new(VecModel::from(badges)));
}

fn open_stats(payload: &Value) -> Result<Live, slint::PlatformError> {
    let win = create(Create::Panel, Stats::new)?;
    panel_theme(&win.global::<Theme>(), payload);
    fill_stats(&win, payload);
    let weak = win.as_weak();
    win.on_drag(move || if let Some(w) = weak.upgrade() { drag(w.window()) });
    win.on_close_clicked(|| later(|st| destroy(st, "stats")));
    let weak = win.as_weak();
    win.on_reset(move || {
        let weak = weak.clone();
        later(move |st| {
            let data = crate::stats_reset(st);
            if let Some(w) = weak.upgrade() {
                fill_stats(&w, &data);
            }
        });
    });
    present_panel(win, "stats", "StandBuddy Stats")
}

fn open_settings(payload: &Value) -> Result<Live, slint::PlatformError> {
    let win = create(Create::Panel, Settings::new)?;
    panel_theme(&win.global::<Theme>(), payload);
    crate::settings_ui::setup(&win, payload, || thread_window("StandBuddy Settings").map_or(0, |h| h as isize));
    let weak = win.as_weak();
    win.on_drag(move || if let Some(w) = weak.upgrade() { drag(w.window()) });
    win.on_cancel(|| later(|st| destroy(st, "settings")));
    let (weak, previous) = (win.as_weak(), payload.clone());
    win.on_save(move || {
        if let Some(w) = weak.upgrade() {
            let form = crate::settings_ui::form_json(&w, &previous);
            later(move |st| crate::settings_save(st, form));
        }
    });
    present_panel(win, "settings", "StandBuddy Settings")
}

/// Once a second while the break is up: remaining time and progress (0..1).
pub fn break_progress(left_ms: i64, total_ms: i64) {
    BREAK.with(|b| {
        if let Some(win) = b.borrow().as_ref().and_then(|w| w.upgrade()) {
            win.set_time(format_left(left_ms).into());
            win.set_progress(1.0 - left_ms.max(0) as f32 / total_ms.max(1) as f32);
        }
    });
}

// --- Public window API ------------------------------------------------------------

/// Fullscreen always-on-top overlay. Returns false if it already exists or can't be shown.
pub fn overlay(st: &mut St, label: &str, payload: Value) -> bool {
    if !open(label) {
        return false;
    }
    let opened = match label {
        "break" => Some(open_break(&payload)),
        "sleep-warning" => Some(open_sleep_warning(&payload)),
        "shutdown-prompt" => Some(open_shutdown_prompt(&payload)),
        "game-blocked" => Some(open_game_blocked(&payload)),
        _ => None,
    };
    if let Some(opened) = opened {
        match opened {
            Ok(live) => LIVE.with(|l| l.borrow_mut().insert(label.into(), live)),
            Err(err) => {
                eprintln!("{label} window: {err}");
                OPEN.with(|open| open.borrow_mut().remove(label));
                return false;
            }
        };
        return true; // on_shown comes from the paint probe
    }
    crate::on_shown(st, label);
    true
}

/// Settings / Stats: open it, or bring it to the front if it is already open.
pub fn panel(_st: &mut St, label: &str, payload: Value) {
    let title = if label == "stats" { "StandBuddy Stats" } else { "StandBuddy Settings" };
    if !open(label) {
        if let Some(hwnd) = thread_window(title) {
            unsafe { SetForegroundWindow(hwnd) };
        }
        return;
    }
    let opened = match label {
        "stats" => open_stats(&payload),
        _ => open_settings(&payload),
    };
    match opened {
        Ok(live) => {
            LIVE.with(|l| l.borrow_mut().insert(label.into(), live));
        }
        Err(err) => {
            eprintln!("{label} window: {err}");
            OPEN.with(|open| open.borrow_mut().remove(label));
        }
    }
}

/// Click-through toast in the bottom-right corner (only "game-warning").
pub fn toast(_st: &mut St, label: &str, payload: Value) {
    if !open(label) {
        return;
    }
    match open_toast(&payload) {
        Ok(live) => {
            LIVE.with(|l| l.borrow_mut().insert(label.into(), live));
        }
        Err(err) => {
            eprintln!("{label} window: {err}");
            OPEN.with(|open| open.borrow_mut().remove(label));
        }
    }
}

pub fn destroy(_st: &mut St, label: &str) {
    if OPEN.with(|open| open.borrow_mut().remove(label)) {
        log(label, "close");
    }
    if let Some(live) = LIVE.with(|l| l.borrow_mut().remove(label)) {
        (live.hide)();
        // Drop after the current callback returns; hidden already, so nothing shows meanwhile.
        Timer::single_shot(Duration::ZERO, move || drop(live));
    }
}

/// Create the tray icon; every menu entry calls `on_menu(id)` on the event loop.
pub fn tray_init(on_menu: fn(&str)) -> Result<(), slint::PlatformError> {
    let tray = Tray::new()?;
    tray.on_menu(move |id| on_menu(id.as_str()));
    tray.show()?;
    TRAY.with(|t| *t.borrow_mut() = Some(tray));
    Ok(())
}

pub fn set_paused(paused: bool) {
    TRAY.with(|t| t.borrow().as_ref().map(|tray| tray.set_paused(paused)));
}

pub fn set_tooltip(text: &str) {
    TRAY.with(|t| t.borrow().as_ref().map(|tray| tray.set_tip(text.into())));
}

#[cfg(test)]
mod tests {
    use super::{format_left, near_point, POINT, RECT};

    #[test]
    fn panels_open_beside_the_tray_inside_the_work_area() {
        let work = RECT { left: 0, top: 0, right: 1920, bottom: 1040 }; // taskbar at the bottom
        let tray = POINT { x: 1800, y: 1060 };
        assert_eq!(near_point(tray, (380, 760), work, 12), (1528, 268));
        // Taskbar on the left, tray at its bottom.
        let work = RECT { left: 60, top: 0, right: 1920, bottom: 1080 };
        assert_eq!(near_point(POINT { x: 30, y: 1050 }, (380, 760), work, 12), (72, 308));
        // Taller than the work area: pinned to the top, never above it.
        assert_eq!(near_point(tray, (380, 1200), RECT { left: 0, top: 0, right: 1920, bottom: 1040 }, 12).1, 12);
    }

    #[test]
    fn break_countdown_rounds_up_to_whole_seconds() {
        assert_eq!(format_left(60_000), "1:00");
        assert_eq!(format_left(59_001), "1:00");
        assert_eq!(format_left(1), "0:01");
        assert_eq!(format_left(-5), "0:00");
    }
}
