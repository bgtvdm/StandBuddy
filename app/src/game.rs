//! Game Limiter: one shared daily budget across every listed game, no bypass.
//! Watches the foreground process, accrues play time, warns at 30 and 5 minutes
//! left, then kills the game through an elevated scheduled task.

use crate::{cmd, data, mono, ui, St};
use serde_json::json;
use std::{fs, thread};
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

fn schedule_timers(st: &mut St) {
    st.game.timers.clear();
    let remaining = st.settings.game_limit_hours * 60.0 - today_minutes(st);
    if remaining <= 0.0 {
        return enforce(st);
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
fn apply_pause(st: &mut St, entry: &str) {
    let flagged = st.settings.game_pause_list.iter().any(|e| normalize(e) == normalize(entry));
    if st.game.auto_paused || st.paused || !flagged {
        return;
    }
    crate::pause_timer(st);
    st.game.auto_paused = true;
    crate::update_menu(st);
}

/// Resume reminders if a game auto-paused them (a manual pause is left alone).
fn clear_pause(st: &mut St) {
    if !st.game.auto_paused {
        return;
    }
    st.game.auto_paused = false;
    if st.break_end.is_none() {
        crate::resume_timer(st);
    }
    crate::update_menu(st);
}

fn reset_session(st: &mut St) {
    st.game.session_start = None;
    st.game.current = None;
    st.game.timers.clear();
    set_poll(st, false);
    clear_pause(st);
}

// --- Elevated kill via a Windows Scheduled Task ------------------------------
// Anti-cheat games (Apex under EAC) reject taskkill from a non-elevated process.
// One on-demand task with HighestAvailable run level is registered when the user
// enables the limiter (one UAC prompt) and triggered silently with `schtasks /run`.
// It has no trigger, so it never fires on its own.

// Reads the target from kill-target.txt and kills it only if it is in the allowlist
// baked into the script when the task was registered (one UAC prompt per game-list
// change) and not a system or security process. Nothing that decides what may be
// killed is read from the data folder: it is user-writable, so any process running as
// the user could otherwise widen what the task kills elevated. For the same reason the
// script is embedded in the task's own action (-EncodedCommand), never run from a file.
const KILL_SCRIPT: &str = r#"$ErrorActionPreference = 'SilentlyContinue'
$dir = '{DIR}'
$allowed = @({ALLOWED})
$denied = @({DENIED})
$targetFile = Join-Path $dir 'kill-target.txt'
if (-not (Test-Path $targetFile)) { return }
$img = (Get-Content -Raw -LiteralPath $targetFile).Trim().ToLower()
if ($img -notmatch '^[a-z0-9._-]+\.exe$') { return }
$base = $img -replace '\.exe$', ''
if ($denied -contains $base) { return }
if ($allowed -contains $base) { & taskkill.exe /f /im $img | Out-Null }
"#;

/// Never killed by the elevated task, whatever the game list says (image names without .exe).
const DENIED: &[&str] = &[
    "system", "smss", "csrss", "wininit", "winlogon", "services", "lsass", "lsaiso", "svchost",
    "dwm", "explorer", "fontdrvhost", "spoolsv", "sihost", "taskhostw", "conhost", "ctfmon",
    "msmpeng", "nissrv", "mssense", "securityhealthservice", "securityhealthsystray", "smartscreen",
    "trustedinstaller", "tiworker", "powershell", "pwsh", "cmd", "taskmgr", "mmc", "regedit",
    "schtasks", "taskkill", "standbuddy",
];
/// The script travels base64-encoded twice inside one command line (32,767 chars): 64 names
/// of up to 40 characters fit with room to spare (tested).
const MAX_GAMES: usize = 64;
const MAX_NAME: usize = 40;

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

/// What the elevated task may kill: listed games with plain image names, minus DENIED,
/// sorted (so the same list always yields the same script) and capped at MAX_GAMES.
fn allowlist(games: &[String]) -> Vec<String> {
    let mut names: Vec<String> = games
        .iter()
        .filter_map(|game| image_name(game))
        .filter_map(|image| image.strip_suffix(".exe").map(str::to_string))
        .filter(|name| name.len() <= MAX_NAME && !DENIED.contains(&name.as_str()))
        .collect();
    names.sort();
    names.dedup();
    if names.len() > MAX_GAMES {
        eprintln!("game-limiter: {} games listed; the elevated kill covers the first {MAX_GAMES}", names.len());
        names.truncate(MAX_GAMES);
    }
    names
}

/// PowerShell array items: 'a','b'. Names are [a-z0-9._-] only, so quoting is safe.
fn ps_list<S: AsRef<str>>(names: &[S]) -> String {
    names.iter().map(|n| format!("'{}'", n.as_ref())).collect::<Vec<_>>().join(",")
}

fn kill_script(dir: &str, allowed: &[String]) -> String {
    KILL_SCRIPT
        .replace("{DIR}", &dir.replace('\'', "''"))
        .replace("{ALLOWED}", &ps_list(allowed))
        .replace("{DENIED}", &ps_list(DENIED))
}

/// Base64 of the kill script as the task runs it (`-EncodedCommand`) for this game list.
fn encoded_script(games: &[String]) -> String {
    let dir = data::data_dir().display().to_string();
    base64(&utf16le(&kill_script(&dir, &allowlist(games))))
}

/// The registered kill task exists and carries exactly this script.
fn task_is_ours(encoded: &str) -> bool {
    cmd("schtasks").args(["/query", "/tn", KILL_TASK, "/xml"]).output().is_ok_and(|out| {
        let ascii: Vec<u8> = out.stdout.into_iter().filter(|b| *b != 0).collect();
        String::from_utf8_lossy(&ascii).contains(encoded)
    })
}

/// Run a PowerShell script elevated (one UAC prompt) and wait for it. The script travels
/// inside the elevated process's own command line, never through a file.
fn run_elevated(script: &str) {
    let ps = format!(
        "try {{ $p = Start-Process -FilePath powershell.exe -ArgumentList '-NoProfile','-NonInteractive','-WindowStyle','Hidden','-ExecutionPolicy','Bypass','-EncodedCommand','{}' -Verb RunAs -Wait -WindowStyle Hidden -PassThru; exit $p.ExitCode }} catch {{ exit 1 }}",
        base64(&utf16le(script))
    );
    let _ = cmd("powershell.exe").args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps]).status();
}

/// The elevated script that registers the task. The definition is a literal inside the
/// script: a definition file in the user-writable data folder could be swapped while the
/// UAC prompt is up, and the elevated side would register whatever it then read.
fn register_script(encoded: &str) -> String {
    let args = format!(
        "-NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -EncodedCommand {encoded}"
    );
    let xml = KILL_TASK_XML.replace("{ARGS}", &args);
    format!("$x = @'\n{xml}\n'@\nRegister-ScheduledTask -TaskName '{KILL_TASK}' -Xml $x -Force | Out-Null")
}

/// Register the kill task if it's missing or carries another game list. Runs
/// off-thread: it waits on a UAC prompt.
pub fn ensure_kill_task(games: Vec<String>) {
    if data::is_test_dir() {
        return;
    }
    thread::spawn(move || {
        // Left behind by builds that registered the task from a file.
        let _ = fs::remove_file(data::data_dir().join("kill-task.xml"));
        let encoded = encoded_script(&games);
        // Registered and carrying exactly this script? Anything else is re-registered —
        // a task for another game list, a pre-2.1.2 task that read the list from
        // settings.json, and the Electron-era task, which ran a script file from the data folder.
        if task_is_ours(&encoded) {
            return;
        }
        run_elevated(&register_script(&encoded));
        if task_is_ours(&encoded) {
            return;
        }
        // UAC declined (no task), or a task that is not ours: never keep the latter.
        let exists = cmd("schtasks").args(["/query", "/tn", KILL_TASK]).output().is_ok_and(|o| o.status.success());
        if exists {
            eprintln!("game-limiter: kill task does not match; removing it");
            run_elevated(&format!("Unregister-ScheduledTask -TaskName '{KILL_TASK}' -Confirm:$false"));
        } else {
            eprintln!("game-limiter: kill task was not registered (UAC declined?)");
        }
    });
}

fn kill(st: &St, entry: &str) {
    let Some(image) = image_name(entry) else { return };
    let games = st.settings.game_list.clone();
    thread::spawn(move || {
        // Hand the target to the elevated task — only if it is ours; otherwise fall back
        // to a direct (likely refused for protected games, but logged) taskkill.
        let handed_off = task_is_ours(&encoded_script(&games))
            && fs::write(data::data_dir().join("kill-target.txt"), &image).is_ok()
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

fn enforce(st: &mut St) {
    flush(st, true);
    st.game.limit_reached = true;
    st.game.day_key = data::today();
    let game = st.game.current.take();
    reset_session(st);
    // Keep polling fast for the rest of the day so a relaunch is killed within a
    // minute instead of up to ten.
    set_poll(st, true);
    if let Some(game) = game {
        kill(st, &game);
    }
    show_blocked(st);
}

fn handle_foreground(st: &mut St, process: &str) {
    if !st.settings.game_limiter_enabled {
        return;
    }
    let day_changed = ensure_day(st);
    let matched = match_game(st, process);

    // Limit already reached today: kill any relaunch of a listed game.
    if st.game.limit_reached {
        if st.game.session_start.is_some() {
            reset_session(st);
        }
        if let Some(game) = matched {
            kill(st, &game);
            show_blocked(st);
        }
        return;
    }

    match matched {
        Some(game) if st.game.session_start.is_none() => {
            st.game.session_start = Some(mono());
            st.game.current = Some(game.clone());
            set_poll(st, true);
            schedule_timers(st);
            // The budget may already be spent (app restarted after the limit): the
            // session was enforced away just now, so there is nothing to pause for.
            if st.game.session_start.is_some() {
                apply_pause(st, &game);
            }
        }
        Some(game) => {
            st.game.current = Some(game);
            flush(st, false);
            // The session crossed midnight: its timers were computed from yesterday's
            // remaining budget — re-plan against the fresh day.
            if day_changed {
                schedule_timers(st);
            }
        }
        None if st.game.session_start.is_some() => {
            flush(st, true);
            reset_session(st);
        }
        // New day: drop the post-limit fast polling.
        None if day_changed => set_poll(st, false),
        None => {}
    }
}

fn show_warning(st: &mut St, minutes_left: u32) {
    let payload = json!({ "minutesLeft": minutes_left, "theme": st.settings.theme });
    ui::toast(st, "game-warning", payload);
    st.game.warning_close = Some(mono() + 7500); // after the toast's slide-out animation
}

fn show_blocked(st: &mut St) {
    // A relaunch after the limit reuses the existing overlay and extends it.
    let payload = json!({ "theme": st.settings.theme, "limitHours": st.settings.game_limit_hours });
    ui::overlay(st, "game-blocked", payload);
    // The game is already killed; no reason to hold the screen for long.
    st.game.blocked_close = Some(mono() + 5500);
}

pub fn start(st: &mut St) {
    if !st.settings.game_limiter_enabled {
        return;
    }
    ensure_day(st);
    set_poll(st, false);
    handle_foreground(st, &foreground_process());
}

pub fn stop(st: &mut St) {
    flush(st, true);
    ui::destroy(st, "game-warning");
    ui::destroy(st, "game-blocked");
    st.game.warning_close = None;
    st.game.blocked_close = None;
    reset_session(st);
    st.game.poll_active = None;
}

pub fn tick(st: &mut St) {
    let now = mono();
    if st.game.warning_close.is_some_and(|at| now >= at) {
        st.game.warning_close = None;
        ui::destroy(st, "game-warning");
    }
    if st.game.blocked_close.is_some_and(|at| now >= at) {
        st.game.blocked_close = None;
        ui::destroy(st, "game-blocked");
    }

    let (due, later): (Vec<_>, Vec<_>) =
        std::mem::take(&mut st.game.timers).into_iter().partition(|(at, _)| *at <= now);
    st.game.timers = later;
    for (_, ev) in due {
        match ev {
            Ev::Warn(minutes) => show_warning(st, minutes),
            Ev::Enforce => enforce(st),
        }
    }

    if let Some(active) = st.game.poll_active {
        if now >= st.game.next_poll {
            st.game.next_poll = now + if active { POLL_ACTIVE_MS } else { POLL_IDLE_MS };
            handle_foreground(st, &foreground_process());
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
        let script = kill_script(r"C:\standbuddy-no-such-folder", &["cs2".into()]);
        let encoded = base64(&utf16le(&script));
        assert!(succeeded(cmd("powershell.exe").args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded])));
    }

    #[test]
    fn allowlist_keeps_listed_games_only_and_never_system_processes() {
        let games: Vec<String> =
            ["CS2.exe", "r5apex", "lsass.exe", "MsMpEng", "bad name.exe", "cs2"].map(String::from).into();
        assert_eq!(allowlist(&games), ["cs2", "r5apex"]);
        assert_eq!(allowlist(&[]), Vec::<String>::new());
        let many: Vec<String> = (0..100).map(|i| format!("game{i:03}")).collect();
        assert_eq!(allowlist(&many).len(), MAX_GAMES);
        // The script depends on the list, so a changed list means "not our task" -> re-register.
        assert_ne!(encoded_script(&["cs2".into()]), encoded_script(&["cs2".into(), "r5apex".into()]));
    }

    /// Runs the real script on a target, with taskkill swapped for an echo.
    fn dry_run(allowed: &[&str], target: &str, tag: &str) -> String {
        let dir = std::env::temp_dir().join(format!("standbuddy-kill-test-{}-{tag}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("kill-target.txt"), target).unwrap();
        let allowed: Vec<String> = allowed.iter().map(|s| s.to_string()).collect();
        let script = kill_script(&dir.display().to_string(), &allowed);
        let echo = script.replace("& taskkill.exe /f /im $img | Out-Null", "Write-Output \"KILL $img\"");
        assert_ne!(echo, script);
        let out = cmd("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &base64(&utf16le(&echo))])
            .output()
            .unwrap();
        let _ = fs::remove_dir_all(&dir);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn kill_script_kills_only_baked_in_games_and_never_denied_ones() {
        assert_eq!(dry_run(&["cs2"], "cs2.exe", "a"), "KILL cs2.exe");
        assert_eq!(dry_run(&["cs2"], "notepad.exe", "b"), "");
        // Even if a denied name reached the baked-in list, the script itself refuses it.
        assert_eq!(dry_run(&["lsass", "cs2"], "lsass.exe", "c"), "");
        assert_eq!(dry_run(&["cs2"], "cs2.exe & calc", "d"), "");
    }

    #[test]
    fn register_script_carries_well_formed_xml_and_fits_a_command_line() {
        // Worst case: MAX_GAMES names of MAX_NAME characters.
        let games: Vec<String> = (0..MAX_GAMES).map(|i| format!("{i:02}{}", "x".repeat(MAX_NAME - 2))).collect();
        assert_eq!(allowlist(&games).len(), MAX_GAMES);
        let encoded = encoded_script(&games);
        let script = register_script(&encoded);
        assert!(script.contains(&encoded));
        // Same script, but parse the definition instead of registering it.
        let check = script.replace(
            &format!("Register-ScheduledTask -TaskName '{KILL_TASK}' -Xml $x -Force | Out-Null"),
            "$null = [xml]$x",
        );
        assert_ne!(check, script);
        assert!(succeeded(cmd("powershell.exe").args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &base64(&utf16le(&check))])));
        // The outer command that starts the elevated PowerShell stays under the 32767-char limit.
        assert!(base64(&utf16le(&script)).len() + 400 < 32_767);
    }

    #[test]
    fn foreground_process_reads_something() {
        // Whatever has focus, the call must not crash and must be lowercase.
        let name = foreground_process();
        assert_eq!(name, name.to_lowercase());
    }
}
