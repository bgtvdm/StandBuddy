//! Game Limiter: one shared daily budget across every listed game, no bypass.
//! Watches the foreground process, accrues play time, warns at 30 and 5 minutes
//! left, then kills the game through an elevated scheduled task.

use crate::{cmd, data, mono, win, St};
use serde_json::json;
use std::{fs, thread};
use tauri::AppHandle;
use windows_sys::Win32::{
    Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
    System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    },
    UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
};

const POLL_IDLE_MS: i64 = 10 * 60 * 1000; // while not gaming
const POLL_ACTIVE_MS: i64 = 60 * 1000; // during a session, and after the limit is hit
const STATS_WRITE_MS: i64 = 5 * 60 * 1000; // throttle disk writes during play
const KILL_TASK: &str = "StandBuddy-KillGame";

enum Ev {
    Warn(u32),
    Enforce,
}

/// All times are awake-time ms (`mono`), so system sleep is never billed as play.
#[derive(Default)]
pub struct Game {
    session_start: Option<i64>,
    current: Option<String>,
    timers: Vec<(i64, Ev)>,
    limit_reached: bool,
    day_key: String,
    pub auto_paused: bool, // break reminders paused because a pause-flagged game runs
    poll_active: Option<bool>, // None = limiter stopped
    next_poll: i64,
    last_write: i64,
    warning_close: Option<i64>,
    blocked_close: Option<i64>,
}

fn normalize(name: &str) -> String {
    let lower = name.to_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

/// The matching gameList entry (e.g. "cs2.exe"), if the process is a listed game.
fn match_game(st: &St, process: &str) -> Option<String> {
    let name = normalize(process);
    if name.is_empty() {
        return None;
    }
    st.settings.game_list.iter().find(|entry| normalize(entry) == name).cloned()
}

/// "name.exe" restricted to safe image-name characters, or None.
fn image_name(entry: &str) -> Option<String> {
    let name = normalize(entry);
    let safe = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c);
    (!name.is_empty() && name.chars().all(safe)).then(|| format!("{name}.exe"))
}

fn today_minutes(st: &St) -> f64 {
    st.stats.days.get(&data::today()).map_or(0.0, |d| d.game_minutes)
}

/// Today's total including the in-progress session (for the tray tooltip).
pub fn live_minutes(st: &St) -> f64 {
    today_minutes(st) + st.game.session_start.map_or(0.0, |s| (mono() - s) as f64 / 60_000.0)
}

/// Reset the "limit reached" flag when the calendar day changes. True on rollover.
fn ensure_day(st: &mut St) -> bool {
    let today = data::today();
    if st.game.day_key == today {
        return false;
    }
    st.game.day_key = today;
    st.game.limit_reached = false;
    true
}

/// Accrue elapsed play time into today's stats and restart the session baseline
/// (crash-safe, no double count). Disk writes are throttled unless forced.
pub fn flush(st: &mut St, force: bool) {
    let Some(start) = st.game.session_start else { return };
    let now = mono();
    let day = st.stats.day();
    day.game_minutes = ((day.game_minutes + (now - start) as f64 / 60_000.0) * 10.0).round() / 10.0;
    st.game.session_start = Some(now);
    if force || now - st.game.last_write >= STATS_WRITE_MS {
        st.game.last_write = now;
        st.save_stats();
    }
}

fn set_poll(st: &mut St, active: bool) {
    if st.game.poll_active == Some(active) {
        return;
    }
    st.game.poll_active = Some(active);
    st.game.next_poll = mono() + if active { POLL_ACTIVE_MS } else { POLL_IDLE_MS };
}

fn schedule_timers(app: &AppHandle, st: &mut St) {
    st.game.timers.clear();
    let remaining = st.settings.game_limit_hours * 60.0 - today_minutes(st);
    if remaining <= 0.0 {
        return enforce(app, st);
    }
    let at = |minutes: f64| mono() + (minutes.max(0.0) * 60_000.0) as i64;
    if remaining > 30.0 {
        st.game.timers.push((at(remaining - 30.0), Ev::Warn(30)));
    }
    if remaining > 5.0 {
        st.game.timers.push((at(remaining - 5.0), Ev::Warn(5)));
    }
    st.game.timers.push((at(remaining), Ev::Enforce));
}

/// Pause break reminders for a pause-flagged game; a manual pause is left alone.
fn apply_pause(app: &AppHandle, st: &mut St, entry: &str) {
    let flagged = st.settings.game_pause_list.iter().any(|e| normalize(e) == normalize(entry));
    if st.game.auto_paused || st.paused || !flagged {
        return;
    }
    crate::pause_timer(st);
    st.game.auto_paused = true;
    crate::update_menu(app, st);
}

/// Resume reminders if a game auto-paused them (a manual pause is left alone).
fn clear_pause(app: &AppHandle, st: &mut St) {
    if !st.game.auto_paused {
        return;
    }
    st.game.auto_paused = false;
    if st.break_end.is_none() {
        crate::resume_timer(st);
    }
    crate::update_menu(app, st);
}

fn reset_session(app: &AppHandle, st: &mut St) {
    st.game.session_start = None;
    st.game.current = None;
    st.game.timers.clear();
    set_poll(st, false);
    clear_pause(app, st);
}

// --- Elevated kill via a Windows Scheduled Task ------------------------------
// Anti-cheat games (Apex under EAC) reject taskkill from a non-elevated process.
// One on-demand task with HighestAvailable run level is registered when the user
// enables the limiter (one UAC prompt) and triggered silently with `schtasks /run`.
// It has no trigger, so it never fires on its own.

// Reads the target from kill-target.txt, validates it, and cross-checks it against
// gameList in settings.json — a tampered target file can only ever kill a listed game.
// The script is embedded in the task's own action (-EncodedCommand), never run from a
// file: the data folder is user-writable, and a script file there would let any
// process running as the user swap in code that the task then runs elevated.
const KILL_SCRIPT: &str = r#"$ErrorActionPreference = 'SilentlyContinue'
$dir = '{DIR}'
$targetFile = Join-Path $dir 'kill-target.txt'
$cfgFile = Join-Path $dir 'settings.json'
if (-not (Test-Path $targetFile)) { return }
$img = (Get-Content -Raw -LiteralPath $targetFile).Trim().ToLower()
if ($img -notmatch '^[a-z0-9._-]+\.exe$') { return }
$base = $img -replace '\.exe$', ''
$allowed = $false
try {
  $cfg = Get-Content -Raw -LiteralPath $cfgFile | ConvertFrom-Json
  foreach ($g in $cfg.gameList) {
    if ((([string]$g) -replace '\.exe$', '').ToLower() -eq $base) { $allowed = $true; break }
  }
} catch { }
if ($allowed) { & taskkill.exe /f /im $img | Out-Null }
"#;

const KILL_TASK_XML: &str = r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>StandBuddy game-limiter enforcement (kills a listed game when the daily limit is reached).</Description>
  </RegistrationInfo>
  <Triggers />
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <ExecutionTimeLimit>PT1M</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>powershell.exe</Command>
      <Arguments>{ARGS}</Arguments>
    </Exec>
  </Actions>
</Task>"#;

fn utf16le(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let bits = chunk.iter().fold(0u32, |acc, b| (acc << 8) | *b as u32) << (8 * (3 - chunk.len()));
        for i in 0..4 {
            let sextet = (bits >> (18 - 6 * i)) & 63;
            out.push(if i <= chunk.len() { TABLE[sextet as usize] as char } else { '=' });
        }
    }
    out
}

fn succeeded(command: &mut std::process::Command) -> bool {
    command.output().is_ok_and(|out| out.status.success())
}

/// Register the kill task if it's missing. Runs off-thread: it waits on a UAC prompt.
pub fn ensure_kill_task() {
    if data::is_test_dir() {
        return;
    }
    thread::spawn(|| {
        let dir = data::data_dir();
        let script = KILL_SCRIPT.replace("{DIR}", &dir.display().to_string().replace('\'', "''"));
        let encoded = base64(&utf16le(&script));
        // Registered and carrying exactly this script? Anything else is re-registered —
        // including the Electron-era task, which ran a script file from the data folder.
        let current = || {
            cmd("schtasks").args(["/query", "/tn", KILL_TASK, "/xml"]).output().is_ok_and(|out| {
                let ascii: Vec<u8> = out.stdout.into_iter().filter(|b| *b != 0).collect();
                String::from_utf8_lossy(&ascii).contains(&encoded)
            })
        };
        if current() {
            return;
        }
        let args = format!(
            "-NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -EncodedCommand {encoded}"
        );
        // schtasks expects UTF-16LE XML with a BOM.
        let xml = dir.join("kill-task.xml");
        let task = format!("{}{}", '\u{FEFF}', KILL_TASK_XML.replace("{ARGS}", &args));
        if fs::write(&xml, utf16le(&task)).is_err() {
            return eprintln!("game-limiter: failed to write the kill task definition");
        }
        let create = format!(r#"/create /tn {KILL_TASK} /xml "{}" /f"#, xml.display()).replace('\'', "''");
        let ps = format!(
            "try {{ $p = Start-Process -FilePath schtasks.exe -ArgumentList '{create}' -Verb RunAs -Wait -WindowStyle Hidden -PassThru; exit $p.ExitCode }} catch {{ exit 1 }}"
        );
        let _ = cmd("powershell.exe")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps])
            .status();
        // Also catches a definition file swapped while the UAC prompt was up.
        if !current() {
            eprintln!("game-limiter: kill task was not registered as written (UAC declined?)");
        }
    });
}

fn kill(entry: &str) {
    let Some(image) = image_name(entry) else { return };
    thread::spawn(move || {
        // Hand the target to the elevated task; fall back to a direct (likely
        // refused for protected games, but logged) taskkill if that isn't possible.
        let handed_off = fs::write(data::data_dir().join("kill-target.txt"), &image).is_ok()
            && succeeded(cmd("schtasks").args(["/run", "/tn", KILL_TASK]));
        if !handed_off {
            match cmd("taskkill").args(["/f", "/im", &image]).output() {
                Ok(out) if out.status.success() => {}
                Ok(out) => eprintln!(
                    "game-limiter: taskkill failed for {image}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
                Err(err) => eprintln!("game-limiter: taskkill failed for {image}: {err}"),
            }
        }
    });
}

fn enforce(app: &AppHandle, st: &mut St) {
    flush(st, true);
    st.game.limit_reached = true;
    st.game.day_key = data::today();
    let game = st.game.current.take();
    reset_session(app, st);
    // Keep polling fast for the rest of the day so a relaunch is killed within a
    // minute instead of up to ten.
    set_poll(st, true);
    if let Some(game) = game {
        kill(&game);
    }
    show_blocked(app, st);
}

fn handle_foreground(app: &AppHandle, st: &mut St, process: &str) {
    if !st.settings.game_limiter_enabled {
        return;
    }
    let day_changed = ensure_day(st);
    let matched = match_game(st, process);

    // Limit already reached today: kill any relaunch of a listed game.
    if st.game.limit_reached {
        if st.game.session_start.is_some() {
            reset_session(app, st);
        }
        if let Some(game) = matched {
            kill(&game);
            show_blocked(app, st);
        }
        return;
    }

    match matched {
        Some(game) if st.game.session_start.is_none() => {
            st.game.session_start = Some(mono());
            st.game.current = Some(game.clone());
            set_poll(st, true);
            schedule_timers(app, st);
            // The budget may already be spent (app restarted after the limit): the
            // session was enforced away just now, so there is nothing to pause for.
            if st.game.session_start.is_some() {
                apply_pause(app, st, &game);
            }
        }
        Some(game) => {
            st.game.current = Some(game);
            flush(st, false);
            // The session crossed midnight: its timers were computed from yesterday's
            // remaining budget — re-plan against the fresh day.
            if day_changed {
                schedule_timers(app, st);
            }
        }
        None if st.game.session_start.is_some() => {
            flush(st, true);
            reset_session(app, st);
        }
        // New day: drop the post-limit fast polling.
        None if day_changed => set_poll(st, false),
        None => {}
    }
}

fn show_warning(app: &AppHandle, st: &mut St, minutes_left: u32) {
    let payload = json!({ "minutesLeft": minutes_left, "theme": st.settings.theme });
    win::toast(app, st, "game-warning", payload);
    st.game.warning_close = Some(mono() + 7500); // after the toast's slide-out animation
}

fn show_blocked(app: &AppHandle, st: &mut St) {
    // A relaunch after the limit reuses the existing overlay and extends it.
    let payload = json!({ "theme": st.settings.theme, "limitHours": st.settings.game_limit_hours });
    win::overlay(app, st, "game-blocked", payload);
    // The game is already killed; no reason to hold the screen for long.
    st.game.blocked_close = Some(mono() + 5500);
}

pub fn start(app: &AppHandle, st: &mut St) {
    if !st.settings.game_limiter_enabled {
        return;
    }
    ensure_day(st);
    set_poll(st, false);
    handle_foreground(app, st, &foreground_process());
}

pub fn stop(app: &AppHandle, st: &mut St) {
    flush(st, true);
    win::destroy(app, st, "game-warning");
    win::destroy(app, st, "game-blocked");
    st.game.warning_close = None;
    st.game.blocked_close = None;
    reset_session(app, st);
    st.game.poll_active = None;
}

pub fn tick(app: &AppHandle, st: &mut St) {
    let now = mono();
    if st.game.warning_close.is_some_and(|at| now >= at) {
        st.game.warning_close = None;
        win::destroy(app, st, "game-warning");
    }
    if st.game.blocked_close.is_some_and(|at| now >= at) {
        st.game.blocked_close = None;
        win::destroy(app, st, "game-blocked");
    }

    let (due, later): (Vec<_>, Vec<_>) =
        std::mem::take(&mut st.game.timers).into_iter().partition(|(at, _)| *at <= now);
    st.game.timers = later;
    for (_, ev) in due {
        match ev {
            Ev::Warn(minutes) => show_warning(app, st, minutes),
            Ev::Enforce => enforce(app, st),
        }
    }

    if let Some(active) = st.game.poll_active {
        if now >= st.game.next_poll {
            st.game.next_poll = now + if active { POLL_ACTIVE_MS } else { POLL_IDLE_MS };
            handle_foreground(app, st, &foreground_process());
        }
    }
}

/// Executable name of the process owning the foreground window (lowercase), or "".
/// Reads the process snapshot rather than opening the process, so it also works
/// for elevated and anti-cheat-protected games.
fn foreground_process() -> String {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(GetForegroundWindow(), &mut pid);
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if pid == 0 || snapshot == INVALID_HANDLE_VALUE {
            return String::new();
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut name = String::new();
        let mut more = Process32FirstW(snapshot, &mut entry);
        while more != 0 {
            if entry.th32ProcessID == pid {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                name = String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase();
                break;
            }
            more = Process32NextW(snapshot, &mut entry);
        }
        CloseHandle(snapshot);
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_name_rejects_anything_but_a_plain_exe_name() {
        assert_eq!(image_name("CS2.exe").as_deref(), Some("cs2.exe"));
        assert_eq!(image_name("r5apex").as_deref(), Some("r5apex.exe"));
        assert_eq!(image_name("a.exe & del x"), None);
        assert_eq!(image_name(""), None);
    }

    #[test]
    fn base64_matches_the_standard_vectors() {
        for (plain, encoded) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foobar", "Zm9vYmFy")] {
            assert_eq!(base64(plain.as_bytes()), encoded);
        }
        // What PowerShell's -EncodedCommand expects: base64 of UTF-16LE.
        assert_eq!(base64(&utf16le("hi")), "aABpAA==");
    }

    #[test]
    fn kill_script_runs_as_an_encoded_command() {
        // No kill-target.txt in this folder, so the script parses, then returns at once.
        let script = KILL_SCRIPT.replace("{DIR}", r"C:\standbuddy-no-such-folder");
        let encoded = base64(&utf16le(&script));
        assert!(succeeded(cmd("powershell.exe").args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded])));
    }

    #[test]
    fn foreground_process_reads_something() {
        // Whatever has focus, the call must not crash and must be lowercase.
        let name = foreground_process();
        assert_eq!(name, name.to_lowercase());
    }
}
