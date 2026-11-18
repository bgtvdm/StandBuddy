#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! StandBuddy — tray app: break overlay every N minutes, bedtime shutdown, daily
//! game budget. One state struct behind a mutex, driven by a one-second tick.
//!
//! Locking rule: the main (UI) thread never waits on the state mutex — commands
//! are async, tray clicks hop to a thread — because code holding the mutex creates
//! windows, which waits on the main thread.

mod data;
mod game;
mod sleep;
mod steam;
mod win;

use data::{Settings, Stats};
use serde_json::{json, Map, Value};
use std::{
    collections::{HashMap, HashSet}, os::windows::process::CommandExt, process::Command, sync::Mutex, thread,
    time::Duration,
};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, RunEvent, WebviewWindow, WindowEvent, Wry,
};
use windows_sys::Win32::{
    Foundation::{GetLastError, ERROR_ALREADY_EXISTS},
    System::{Threading::CreateMutexW, WindowsProgramming::QueryUnbiasedInterruptTime},
};

pub struct St {
    pub settings: Settings,
    pub stats: Stats,
    tips: Vec<Value>,
    pub paused: bool,
    next_break: Option<i64>,        // mono ms
    pub break_end: Option<i64>,     // mono ms; Some = the break overlay is up
    pub break_started: Option<i64>, // mono ms; set when the overlay is actually shown
    pub sleep_events: Vec<(i64, sleep::Ev)>, // wall ms
    pub bedtime: i64,
    pub snoozes: u32,
    pub game: game::Game,
    pub payloads: HashMap<String, Value>, // what each open window loads, by label
    pub wanted: HashSet<String>,          // panels the user has open (shown when painted)
    pub warm_until: i64,                  // mono ms; hidden panels are destroyed after this
    tooltip: String,
}

struct PauseItem(MenuItem<Wry>);

pub fn with<R>(app: &AppHandle, f: impl FnOnce(&mut St) -> R) -> R {
    let state = app.state::<Mutex<St>>();
    let mut guard = state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut guard)
}

/// Milliseconds the machine has been awake. Interval timers use this, so system
/// sleep pauses them instead of firing a break the moment the lid opens.
pub fn mono() -> i64 {
    let mut ticks = 0u64; // 100 ns units
    unsafe { QueryUnbiasedInterruptTime(&mut ticks) };
    (ticks / 10_000) as i64
}

/// Wall-clock ms, for deadlines tied to a time of day.
pub fn wall() -> i64 {
    chrono::Local::now().timestamp_millis()
}

/// A child process that doesn't flash a console window.
pub fn cmd(program: &str) -> Command {
    let mut command = Command::new(program);
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    command
}

fn format_countdown(ms: i64) -> String {
    if ms <= 0 {
        return "0:00".into();
    }
    let total = (ms + 999) / 1000;
    let (h, m, s) = (total / 3600, total % 3600 / 60, total % 60);
    if h >= 1 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

impl St {
    fn load() -> St {
        St {
            settings: data::load("settings.json"),
            stats: data::load("stats.json"),
            tips: serde_json::from_str(include_str!("../../tips.json")).unwrap_or_default(),
            paused: false,
            next_break: None,
            break_end: None,
            break_started: None,
            sleep_events: vec![],
            bedtime: 0,
            snoozes: 3,
            game: game::Game::default(),
            payloads: HashMap::new(),
            wanted: HashSet::new(),
            warm_until: 0,
            tooltip: String::new(),
        }
    }

    fn save_settings(&self) {
        data::save("settings.json", &self.settings);
    }

    pub fn save_stats(&self) {
        data::save("stats.json", &self.stats);
    }

    /// Roll the "breaks today" counter over at midnight.
    fn ensure_today(&mut self) {
        let today = data::today();
        if self.settings.today_date != today {
            self.settings.today_date = today;
            self.settings.today_breaks = 0;
            self.save_settings();
        }
    }

    fn next_tip(&mut self) -> Value {
        if self.tips.is_empty() {
            return Value::Null;
        }
        let index = self.settings.next_tip_index % self.tips.len();
        self.settings.next_tip_index = (index + 1) % self.tips.len();
        self.save_settings();
        self.tips[index].clone()
    }

    pub fn record_break_shown(&mut self) {
        self.stats.touch_used_day();
        self.stats.day().shown += 1;
        self.break_started = Some(mono());
        self.save_stats();
    }

    fn record_break_completed(&mut self) {
        self.ensure_today();
        self.settings.today_breaks += 1;
        let today = data::today();
        if !self.settings.active_days.contains(&today) {
            self.settings.active_days.push(today);
        }
        self.save_settings();

        self.stats.touch_used_day();
        let elapsed = self.break_started.take().map(|start| mono() - start);
        let day = self.stats.day();
        day.completed += 1;
        if let Some(ms) = elapsed {
            day.break_minutes += (ms as f64 / 60_000.0 * 10.0).round() / 10.0;
        }
        self.stats.total_completed += 1;
        self.stats.unlock_achievements();
        self.save_stats();
    }

    fn record_emergency_skip(&mut self) {
        self.stats.touch_used_day();
        self.stats.day().emergency += 1;
        self.break_started = None;
        self.save_stats();
    }
}

// --- Break timer --------------------------------------------------------------

fn start_interval(st: &mut St) {
    st.next_break = Some(mono() + (st.settings.interval * 60_000.0) as i64);
}

pub fn pause_timer(st: &mut St) {
    st.paused = true;
    st.next_break = None;
}

pub fn resume_timer(st: &mut St) {
    st.paused = false;
    start_interval(st);
}

fn show_break(app: &AppHandle, st: &mut St) {
    if st.break_end.is_some() {
        return;
    }
    st.next_break = None;
    st.break_started = None;
    let payload = json!({
        "duration": st.settings.break_duration,
        "tip": st.next_tip(),
        "theme": st.settings.theme,
        "emergencyUnlock": st.settings.emergency_unlock,
    });
    if win::overlay(app, st, "break", payload) {
        st.break_end = Some(mono() + (st.settings.break_duration * 1000.0) as i64);
    } else {
        start_interval(st); // couldn't show it: don't count a break nobody saw
    }
}

fn close_break(app: &AppHandle, st: &mut St, completed: bool) {
    st.break_end = None;
    win::destroy(app, st, "break");
    if completed {
        st.record_break_completed();
    }
    if !st.paused {
        start_interval(st);
    }
}

// --- Tray ---------------------------------------------------------------------

pub fn update_menu(app: &AppHandle, st: &St) {
    let _ = app.state::<PauseItem>().0.set_text(if st.paused { "Resume" } else { "Pause" });
}

fn update_tooltip(app: &AppHandle, st: &mut St) {
    st.ensure_today();
    let countdown = match st.next_break {
        _ if st.break_end.is_some() => "On break".into(),
        Some(at) if !st.paused => format_countdown(at - mono()),
        _ if st.game.auto_paused => "Paused (gaming)".into(),
        _ => "Paused".to_string(),
    };
    let plural = |word: &str, n: u32| if n == 1 { word.to_string() } else { format!("{word}s") };
    let (breaks, streak) = (st.settings.today_breaks, st.stats.streak());
    let mut text = format!(
        "StandBuddy\r\nNext break in: {countdown}\r\nToday: {breaks} {}\r\nActive streak: {streak} {}",
        plural("break", breaks),
        plural("day", streak)
    );
    if st.settings.game_limiter_enabled {
        let used = game::live_minutes(st) as i64;
        let used = if used >= 60 { format!("{}h {}m", used / 60, used % 60) } else { format!("{used}m") };
        text += &format!("\r\nGame time today: {used} / {}h", st.settings.game_limit_hours);
    }
    if text != st.tooltip {
        if let Some(tray) = app.tray_by_id("main") {
            let _ = tray.set_tooltip(Some(&text));
        }
        st.tooltip = text;
    }
}

fn on_menu(app: &AppHandle, id: &str) {
    match id {
        "pause" => with(app, |st| {
            st.game.auto_paused = false; // a manual pause/resume takes over from a game's
            if st.paused { resume_timer(st) } else { pause_timer(st) }
            update_menu(app, st);
        }),
        "stats" | "settings" => with(app, |st| open_panel(app, st, id, false)),
        "quit" => {
            with(app, |st| game::flush(st, true));
            app.exit(0);
        }
        _ => {}
    }
}

/// How long hidden Settings/Stats stay loaded after the last tray hover or close.
pub const WARM_MS: i64 = 30_000;

/// The cursor reached the tray icon: load both panels hidden now, so a Settings or
/// Stats click a moment later only shows them. Idle, nothing stays loaded (see tick).
fn warm_up(app: &AppHandle) {
    with(app, |st| {
        st.warm_until = mono() + WARM_MS;
        open_panel(app, st, "settings", true);
        open_panel(app, st, "stats", true);
    });
}

/// Open Settings or Stats with fresh data; `prewarm` only creates it hidden.
fn open_panel(app: &AppHandle, st: &mut St, label: &str, prewarm: bool) {
    let (title, w, h, payload) = if label == "stats" {
        let mut payload = st.stats.for_window(st.settings.interval);
        payload["theme"] = json!(st.settings.theme);
        payload["dark"] = json!(st.settings.dark);
        ("StandBuddy Stats", 480.0, 720.0, payload)
    } else {
        ("StandBuddy Settings", 380.0, 760.0, serde_json::to_value(&st.settings).unwrap_or_default())
    };
    if prewarm {
        win::prewarm(app, st, label, title, w, h, payload);
    } else {
        win::panel(app, st, label, title, w, h, payload);
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let item = |id: &str, text: &str| MenuItem::with_id(app, id, text, true, None::<&str>);
    let pause = item("pause", "Pause")?;
    let menu = Menu::with_items(
        app,
        &[
            &pause,
            &item("stats", "Stats")?,
            &item("settings", "Settings...")?,
            &PredefinedMenuItem::separator(app)?,
            &item("quit", "Quit")?,
        ],
    )?;
    app.manage(PauseItem(pause));
    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().expect("bundle icon").clone())
        .tooltip("StandBuddy")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if matches!(event, TrayIconEvent::Enter { .. } | TrayIconEvent::Click { .. }) {
                let app = tray.app_handle().clone();
                thread::spawn(move || warm_up(&app));
            }
        })
        .on_menu_event(|app, event| {
            let (app, id) = (app.clone(), event.id().0.clone());
            thread::spawn(move || on_menu(&app, &id));
        })
        .build(app)?;
    Ok(())
}

// --- Commands (called from the windows through shim.js) -------------------------

#[tauri::command(async)]
fn get_payload(app: AppHandle, window: WebviewWindow) -> Value {
    with(&app, |st| st.payloads.get(window.label()).cloned().unwrap_or_default())
}

#[tauri::command(async)]
fn reveal(app: AppHandle, window: WebviewWindow) {
    with(&app, |st| win::reveal(&app, st, window.label()));
}

#[tauri::command(async)]
fn close_self(app: AppHandle, window: WebviewWindow) {
    if win::is_panel(window.label()) {
        with(&app, |st| win::hide_panel(&app, st, window.label()));
    } else if !win::is_overlay(window.label()) {
        with(&app, |st| win::destroy(&app, st, window.label()));
    }
}

// Frameless Settings/Stats windows drag by their own header.
#[tauri::command]
fn drag_self(window: WebviewWindow) {
    let _ = window.start_dragging();
}

#[tauri::command(async)]
fn break_skip(app: AppHandle) {
    with(&app, |st| {
        // Refused here too, not only hidden in the page.
        if st.break_end.is_some() && st.settings.emergency_unlock {
            st.record_emergency_skip();
            close_break(&app, st, false);
        }
    });
}

#[tauri::command(async)]
fn stats_reset(app: AppHandle) -> Value {
    with(&app, |st| {
        let _ = std::fs::remove_file(data::data_dir().join("stats.json"));
        st.stats = Stats::default();
        // The tray counts today's breaks from settings — reset that too.
        st.settings.today_breaks = 0;
        st.save_settings();
        st.stats.for_window(st.settings.interval)
    })
}

#[tauri::command(async)]
fn settings_save(app: AppHandle, new_settings: Value) {
    let new = &new_settings;
    let num = |key: &str, default: f64| new.get(key).and_then(Value::as_f64).unwrap_or(default);
    let flag = |key: &str| new.get(key).and_then(Value::as_bool).unwrap_or(false);
    let list = |key: &str| -> Vec<String> {
        let items = new.get(key).and_then(Value::as_array);
        items.map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
    };
    let text = |key: &str| new.get(key).and_then(Value::as_str).filter(|s| !s.is_empty());

    let (launch_at_login, limiter_on) = with(&app, |st| {
        let s = &mut st.settings;
        let interval = num("interval", s.interval).max(1.0);
        let interval_changed = interval != s.interval;
        s.interval = interval;
        s.break_duration = num("breakDuration", s.break_duration).max(10.0);
        s.launch_at_login = flag("launchAtLogin");
        s.sleep_mode_enabled = flag("sleepModeEnabled");
        s.sleep_mode_bedtime = text("sleepModeBedtime").unwrap_or("22:00").into();
        s.theme = text("theme").unwrap_or("warm").into();
        s.dark = flag("dark");
        s.emergency_unlock = flag("emergencyUnlock");
        s.game_limiter_enabled = flag("gameLimiterEnabled");
        s.game_limit_hours = Some(num("gameLimitHours", 4.0)).filter(|h| *h > 0.0).unwrap_or(4.0);
        s.game_list = list("gameList");
        s.game_names = new.get("gameNames").and_then(Value::as_object).cloned().unwrap_or_else(Map::new);
        // Pause-reminders list: lowercased and pruned to exes still in gameList.
        let listed: Vec<String> = s.game_list.iter().map(|e| e.to_lowercase()).collect();
        s.game_pause_list =
            list("gamePauseList").iter().map(|e| e.to_lowercase()).filter(|e| listed.contains(e)).collect();
        st.save_settings();

        // Only restart the countdown when the interval actually changed — a theme or
        // game-list save must not reset progress toward the next break.
        if interval_changed && !st.paused && st.break_end.is_none() {
            start_interval(st);
        }
        sleep::schedule(st);
        game::stop(&app, st);
        game::start(&app, st);
        win::hide_panel(&app, st, "settings");
        (st.settings.launch_at_login, st.settings.game_limiter_enabled)
    });
    sync_autostart(launch_at_login);
    if limiter_on {
        game::ensure_kill_task(); // one UAC prompt, only if the task is missing
    }
}

#[tauri::command(async)]
fn sleep_warning_close(app: AppHandle) {
    with(&app, |st| win::destroy(&app, st, "sleep-warning"));
}

#[tauri::command(async)]
fn shutdown_snooze(app: AppHandle, snoozes_left: u32) {
    with(&app, |st| sleep::snooze(&app, st, snoozes_left));
}

#[tauri::command(async)]
fn shutdown_execute(app: AppHandle) {
    with(&app, |st| sleep::shutdown(&app, st));
}

#[tauri::command(async)]
fn game_pick_exe(window: WebviewWindow) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = window
        .dialog()
        .file()
        .set_parent(&window)
        .set_title("Select a game executable")
        .add_filter("Executables", &["exe"])
        .blocking_pick_file()?;
    let path = picked.into_path().ok()?;
    Some(path.file_name()?.to_string_lossy().to_lowercase())
}

#[tauri::command(async)]
fn steam_list_games() -> Vec<steam::SteamGame> {
    steam::list_games()
}

// --- App ------------------------------------------------------------------------

/// Launch-at-login: a Run value pointing at this exe.
fn sync_autostart(enabled: bool) {
    if data::is_test_dir() {
        return;
    }
    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    let mut reg = cmd("reg");
    match (enabled, std::env::current_exe()) {
        (true, Ok(exe)) => {
            let quoted = format!("\"{}\"", exe.display());
            reg.args(["add", KEY, "/v", "StandBuddy", "/t", "REG_SZ", "/d", &quoted, "/f"]);
        }
        _ => {
            reg.args(["delete", KEY, "/v", "StandBuddy", "/f"]);
        }
    }
    let _ = reg.output();
}

/// A second instance would mean two trays, duplicate overlays and double game-time
/// accounting against the same stats.json. The mutex lives until the process exits.
fn is_only_instance() -> bool {
    let name = if data::is_test_dir() { "StandBuddy-Test" } else { "StandBuddy-SingleInstance" };
    let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
    unsafe {
        CreateMutexW(std::ptr::null(), 1, wide.as_ptr());
        GetLastError() != ERROR_ALREADY_EXISTS
    }
}

/// Test mode only: `STANDBUDDY_OPEN=settings,break,…` opens windows without the tray,
/// so every window can be smoke-tested and timed (see the timing lines in win.rs).
fn open_for_test(app: &AppHandle) {
    let Some(list) = std::env::var("STANDBUDDY_OPEN").ok().filter(|_| data::is_test_dir()) else { return };
    for label in list.split(',') {
        match label {
            "settings" | "stats" => on_menu(app, label),
            // `warm` (tray hover), `wait[:ms]` (default 2 s) and `hide-<panel>` time
            // the warm path and the cool-down.
            "warm" => warm_up(app),
            _ if label.starts_with("wait") => {
                let ms = label.strip_prefix("wait:").and_then(|v| v.parse().ok()).unwrap_or(2000);
                thread::sleep(Duration::from_millis(ms));
            }
            "hide-settings" | "hide-stats" => with(app, |st| win::hide_panel(app, st, &label[5..])),
            "break" => with(app, |st| show_break(app, st)),
            _ => with(app, |st| {
                let payload = json!({
                    "theme": st.settings.theme, "minutesLeft": 5, "snoozesLeft": 3,
                    "limitHours": st.settings.game_limit_hours,
                });
                if label == "game-warning" {
                    win::toast(app, st, label, payload);
                } else if win::is_overlay(label) {
                    win::overlay(app, st, label, payload);
                }
            }),
        }
    }
}

fn tick(app: &AppHandle, st: &mut St) {
    let now = mono();
    if st.break_end.is_some_and(|at| now >= at) {
        close_break(app, st, true);
    }
    if st.next_break.is_some_and(|at| now >= at) {
        show_break(app, st);
    }
    // Cool-down: no panel open and no recent tray hover — free the WebView entirely.
    if now >= st.warm_until && st.wanted.is_empty() {
        win::destroy(app, st, "settings");
        win::destroy(app, st, "stats");
    }
    sleep::tick(app, st);
    game::tick(app, st);
    update_tooltip(app, st);
}

fn main() {
    if !is_only_instance() {
        return;
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Mutex::new(St::load()))
        .invoke_handler(tauri::generate_handler![
            get_payload,
            reveal,
            close_self,
            drag_self,
            break_skip,
            stats_reset,
            settings_save,
            sleep_warning_close,
            shutdown_snooze,
            shutdown_execute,
            game_pick_exe,
            steam_list_games
        ])
        .on_window_event(|window, event| {
            // Alt+F4 on Settings/Stats hides it (kept warm) instead of destroying it.
            // Off the main thread: it must never wait on the state mutex.
            if win::is_panel(window.label()) {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let (app, label) = (window.app_handle().clone(), window.label().to_string());
                    thread::spawn(move || with(&app, |st| win::hide_panel(&app, st, &label)));
                }
                return;
            }
            if !win::is_overlay(window.label()) {
                return;
            }
            match event {
                // Overlays close only when the app closes them (no Alt+F4).
                WindowEvent::CloseRequested { api, .. } => api.prevent_close(),
                // Hold focus — unless it moved to another of our own windows, which
                // would otherwise turn two overlays into a focus tug-of-war.
                WindowEvent::Focused(false) => {
                    let windows = window.app_handle().webview_windows();
                    if !windows.values().any(|w| w.is_focused().unwrap_or(false)) {
                        let _ = window.set_focus();
                    }
                }
                _ => {}
            }
        })
        .setup(|app| {
            let app = app.handle().clone();
            build_tray(&app)?;
            let launch_at_login = with(&app, |st| {
                start_interval(st);
                sleep::schedule(st);
                game::start(&app, st);
                st.settings.launch_at_login
            });
            thread::spawn(move || sync_autostart(launch_at_login));
            let opener = app.clone();
            thread::spawn(move || open_for_test(&opener));
            thread::spawn(move || loop {
                thread::sleep(Duration::from_secs(1));
                with(&app, |st| tick(&app, st));
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to start StandBuddy")
        .run(|app, event| match event {
            // Closing the last window must not quit a tray app; Quit passes a code.
            RunEvent::ExitRequested { api, code: None, .. } => api.prevent_exit(),
            // try_lock: this is the main thread (see the locking rule above).
            RunEvent::Exit => {
                if let Ok(mut st) = app.state::<Mutex<St>>().try_lock() {
                    game::flush(&mut st, true);
                }
            }
            _ => {}
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countdown_rounds_up_and_adds_hours_only_when_needed() {
        assert_eq!(format_countdown(0), "0:00");
        assert_eq!(format_countdown(1), "0:01");
        assert_eq!(format_countdown(35 * 60_000), "35:00");
        assert_eq!(format_countdown(3_600_000 + 61_000), "1:01:01");
    }

    #[test]
    fn awake_clock_advances() {
        let start = mono();
        thread::sleep(Duration::from_millis(20));
        assert!(mono() > start);
    }
}
