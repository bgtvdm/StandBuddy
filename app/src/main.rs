#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! StandBuddy — tray app: break overlay every N minutes, bedtime shutdown, daily
//! game budget. One state struct on the UI thread, driven by a one-second Slint timer.
//!
//! Threading rule: `St` lives only on the event-loop thread (`with`). Blocking work
//! (registry, schtasks, Steam scan, file dialogs) runs in a `std::thread` and reports
//! back with `slint::invoke_from_event_loop`. Never call `with` from inside `with`.

mod bg;
mod chime;
mod data;
mod emoji;
mod game;
mod settings_ui;
mod sleep;
mod steam;
mod ui;

use data::{Settings, Stats};
use serde_json::{json, Map, Value};
use std::{cell::RefCell, os::windows::process::CommandExt, process::Command, thread, time::Duration};
use windows_sys::Win32::{
    Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM},
    System::{
        LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW},
        Threading::CreateMutexW,
        WindowsProgramming::QueryUnbiasedInterruptTime,
    },
    UI::WindowsAndMessaging::{CreateWindowExW, DefWindowProcW, RegisterClassW, WM_ENDSESSION, WNDCLASSW},
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
    tooltip: String,
}

thread_local! {
    static STATE: RefCell<Option<St>> = const { RefCell::new(None) };
}

/// Run `f` on the app state. Event-loop thread only; not re-entrant.
pub fn with<R>(f: impl FnOnce(&mut St) -> R) -> R {
    STATE.with(|state| f(state.borrow_mut().as_mut().expect("state not loaded")))
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

/// A window reports that it is on screen.
pub fn on_shown(st: &mut St, label: &str) {
    if label == "break" && st.break_started.is_none() {
        st.record_break_shown();
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

fn show_break(st: &mut St) {
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
    if ui::overlay(st, "break", payload) {
        st.break_end = Some(mono() + (st.settings.break_duration * 1000.0) as i64);
    } else {
        start_interval(st); // couldn't show it: don't count a break nobody saw
    }
}

fn close_break(st: &mut St, completed: bool) {
    st.break_end = None;
    ui::destroy(st, "break");
    if completed {
        st.record_break_completed();
    }
    if !st.paused {
        start_interval(st);
    }
}

// --- Tray ---------------------------------------------------------------------

pub fn update_menu(st: &St) {
    ui::set_paused(st.paused);
}

fn update_tooltip(st: &mut St) {
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
        ui::set_tooltip(&text);
        st.tooltip = text;
    }
}

fn on_menu(id: &str) {
    match id {
        "pause" => with(|st| {
            st.game.auto_paused = false; // a manual pause/resume takes over from a game's
            if st.paused { resume_timer(st) } else { pause_timer(st) }
            update_menu(st);
        }),
        "stats" => with(|st| {
            let mut payload = st.stats.for_window(st.settings.interval);
            payload["theme"] = json!(st.settings.theme);
            payload["dark"] = json!(st.settings.dark);
            ui::panel(st, "stats", payload);
        }),
        "settings" => with(|st| {
            let payload = serde_json::to_value(&st.settings).unwrap_or_default();
            ui::panel(st, "settings", payload);
        }),
        "quit" => {
            with(|st| game::flush(st, true));
            let _ = slint::quit_event_loop();
        }
        _ => {}
    }
}

// --- Actions the windows call (wired to Slint callbacks in Phase 3) -----------

pub fn break_skip(st: &mut St) {
    // Refused here too, not only hidden in the window.
    if st.break_end.is_some() && st.settings.emergency_unlock {
        st.record_emergency_skip();
        close_break(st, false);
    }
}

pub fn stats_reset(st: &mut St) -> Value {
    let _ = std::fs::remove_file(data::data_dir().join("stats.json"));
    st.stats = Stats::default();
    // The tray counts today's breaks from settings — reset that too.
    st.settings.today_breaks = 0;
    st.save_settings();
    st.stats.for_window(st.settings.interval)
}

pub fn settings_save(st: &mut St, new_settings: Value) {
    let new = &new_settings;
    let num = |key: &str, default: f64| new.get(key).and_then(Value::as_f64).unwrap_or(default);
    let flag = |key: &str| new.get(key).and_then(Value::as_bool).unwrap_or(false);
    let list = |key: &str| -> Vec<String> {
        let items = new.get(key).and_then(Value::as_array);
        items.map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
    };
    let text = |key: &str| new.get(key).and_then(Value::as_str).filter(|s| !s.is_empty());

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
    game::stop(st);
    game::start(st);
    ui::destroy(st, "settings");
    let (launch_at_login, limiter_on) = (st.settings.launch_at_login, st.settings.game_limiter_enabled);
    let games = st.settings.game_list.clone();
    thread::spawn(move || sync_autostart(launch_at_login));
    if limiter_on {
        game::ensure_kill_task(games); // one UAC prompt, only if the task is missing or the list changed; off-thread
    }
}

pub fn sleep_warning_close(st: &mut St) {
    ui::destroy(st, "sleep-warning");
}

pub fn shutdown_snooze(st: &mut St, snoozes_left: u32) {
    sleep::snooze(st, snoozes_left);
}

pub fn shutdown_execute(st: &mut St) {
    sleep::shutdown(st);
}

// --- App ------------------------------------------------------------------------

/// Launch-at-login: a Run value pointing at this exe. Blocking: call off-thread.
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

/// The tray menu is a plain Win32 popup menu (Slint's tray), which Windows draws light
/// unless the app opts in to dark mode. uxtheme's undocumented ordinals, as Explorer and
/// Notepad++ use them: 135 = SetPreferredAppMode (1 = follow the Windows app theme; on
/// 1809 the same slot is AllowDarkModeForApp(true)), 136 = FlushMenuThemes.
fn follow_windows_menu_theme() {
    let dll: Vec<u16> = "uxtheme.dll".encode_utf16().chain([0]).collect();
    unsafe {
        let module = LoadLibraryW(dll.as_ptr());
        if module.is_null() {
            return;
        }
        if let Some(set_mode) = GetProcAddress(module, 135 as *const u8) {
            let set_mode: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(set_mode);
            set_mode(1);
        }
        if let Some(flush) = GetProcAddress(module, 136 as *const u8) {
            let flush: unsafe extern "system" fn() = std::mem::transmute(flush);
            flush();
        }
    }
}

/// Logoff/shutdown ends the process without leaving the Slint loop, so flush the game
/// clock here, as Quit does. Needs a hidden top-level window: message-only windows get
/// no WM_ENDSESSION. The loop pumps this thread, so the handler runs on the state's thread.
fn watch_session_end() {
    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if msg == WM_ENDSESSION && wparam != 0 {
            // Can arrive while `with` is active (any nested message pump): never panic here.
            STATE.with(|state| {
                if let Some(st) = state.try_borrow_mut().ok().as_mut().and_then(|s| s.as_mut()) {
                    game::flush(st, true);
                    if data::is_test_dir() {
                        eprintln!("session end {}", mono());
                    }
                }
            });
        }
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }
    let class: Vec<u16> = "StandBuddySessionWatch".encode_utf16().chain([0]).collect();
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: instance, lpszClassName: class.as_ptr(), ..std::mem::zeroed() };
        RegisterClassW(&wc);
        // Never shown (no WS_VISIBLE); lives until the process exits.
        CreateWindowExW(0, class.as_ptr(), class.as_ptr(), 0, 0, 0, 0, 0, std::ptr::null_mut(), std::ptr::null_mut(), instance, std::ptr::null());
    }
}

/// Test mode only: `STANDBUDDY_OPEN=settings,break,…` opens windows without the tray.
/// `wait[:ms]` (default 1500) delays the rest; `quit` ends the run, so a scripted smoke
/// test exits on its own.
fn open_for_test() {
    let Some(list) = std::env::var("STANDBUDDY_OPEN").ok().filter(|_| data::is_test_dir()) else { return };
    run_test_labels(list.split(',').map(String::from).collect());
}

fn run_test_labels(labels: Vec<String>) {
    for (i, label) in labels.iter().enumerate() {
        let label = label.as_str();
        if let Some(rest) = label.strip_prefix("wait") {
            let ms = rest.strip_prefix(':').and_then(|ms| ms.parse().ok()).unwrap_or(1500);
            let later = labels[i + 1..].to_vec();
            slint::Timer::single_shot(Duration::from_millis(ms), move || run_test_labels(later));
            return;
        }
        match label {
            "settings" | "stats" | "pause" | "quit" => on_menu(label),
            "break" => with(show_break),
            "break-end" => with(|st| close_break(st, true)),
            _ => with(|st| {
                let payload = json!({
                    "theme": st.settings.theme, "minutesLeft": 5, "snoozesLeft": 3,
                    "limitHours": st.settings.game_limit_hours,
                });
                if label == "game-warning" {
                    ui::toast(st, label, payload);
                } else if ui::is_overlay(label) {
                    ui::overlay(st, label, payload);
                }
            }),
        }
    }
}

fn tick(st: &mut St) {
    let now = mono();
    if let Some(end) = st.break_end {
        if now >= end {
            close_break(st, true);
        } else {
            ui::break_progress(end - now, (st.settings.break_duration * 1000.0) as i64);
        }
    }
    if st.next_break.is_some_and(|at| now >= at) {
        show_break(st);
    }
    sleep::tick(st);
    game::tick(st);
    update_tooltip(st);
}

fn main() {
    if !is_only_instance() {
        return;
    }
    ui::select_backend().expect("failed to start the UI backend");
    STATE.with(|state| *state.borrow_mut() = Some(St::load()));
    follow_windows_menu_theme();
    ui::tray_init(on_menu).expect("failed to create the tray icon");
    let launch_at_login = with(|st| {
        start_interval(st);
        sleep::schedule(st);
        game::start(st);
        update_tooltip(st);
        st.settings.launch_at_login
    });
    thread::spawn(move || sync_autostart(launch_at_login));
    watch_session_end();

    let ticker = slint::Timer::default();
    ticker.start(slint::TimerMode::Repeated, Duration::from_secs(1), || with(tick));
    slint::Timer::single_shot(Duration::ZERO, open_for_test);
    slint::Timer::single_shot(Duration::from_secs(1), ui::warm_up);

    slint::run_event_loop_until_quit().expect("event loop failed");
    with(|st| game::flush(st, true));
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
