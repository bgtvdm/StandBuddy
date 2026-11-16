//! Sleep Mode: warnings 30/15/5 minutes before bedtime, then a shutdown prompt.
//! Deadlines are wall-clock, checked every tick, so a machine that slept through
//! one sees it as overdue on wake and re-validates instead of acting on it blindly.

use crate::{wall, win, St};
use chrono::{Days, Local};
use serde_json::json;
use tauri::AppHandle;

pub enum Ev {
    Warning,
    Prompt { fresh: bool }, // fresh = the bedtime prompt itself, not a snooze
}

// Woke up this long past bedtime (machine slept overnight): don't shut down at 9am,
// re-arm for tonight instead.
const STALE_MS: i64 = 6 * 60 * 60 * 1000;
const SNOOZE_MS: i64 = 5 * 60 * 1000;

/// Next occurrence of "HH:MM" local time, as wall-clock ms.
fn next_bedtime(hhmm: &str) -> Option<i64> {
    let (h, m) = hhmm.split_once(':')?;
    let (h, m) = (h.trim().parse().ok()?, m.trim().parse().ok()?);
    let now = Local::now();
    let at = |day: chrono::NaiveDate| {
        day.and_hms_opt(h, m, 0)?.and_local_timezone(Local).earliest()
    };
    let today = at(now.date_naive())?;
    let bedtime = if today <= now { at(now.date_naive() + Days::new(1))? } else { today };
    Some(bedtime.timestamp_millis())
}

pub fn schedule(st: &mut St) {
    st.sleep_events.clear();
    if !st.settings.sleep_mode_enabled {
        return;
    }
    let Some(bedtime) = next_bedtime(&st.settings.sleep_mode_bedtime) else { return };
    let now = wall();
    for minutes in [30, 15, 5] {
        let at = bedtime - minutes * 60_000;
        if at > now {
            st.sleep_events.push((at, Ev::Warning));
        }
    }
    st.sleep_events.push((bedtime, Ev::Prompt { fresh: true }));
    st.bedtime = bedtime;
}

pub fn tick(app: &AppHandle, st: &mut St) {
    let now = wall();
    let (due, later): (Vec<_>, Vec<_>) =
        std::mem::take(&mut st.sleep_events).into_iter().partition(|(at, _)| *at <= now);
    st.sleep_events = later;
    for (at, ev) in due {
        match ev {
            Ev::Warning => {
                // Fired late: show the real remaining time, skip once bedtime has come.
                let minutes_left = ((st.bedtime - now) as f64 / 60_000.0).round() as i64;
                if minutes_left >= 1 {
                    let payload = json!({ "minutesLeft": minutes_left, "theme": st.settings.theme });
                    win::overlay(app, st, "sleep-warning", payload);
                }
            }
            Ev::Prompt { fresh } => {
                if now - at > STALE_MS {
                    schedule(st);
                    return;
                }
                if fresh {
                    st.snoozes = 3;
                }
                let payload = json!({ "snoozesLeft": st.snoozes, "theme": st.settings.theme });
                win::overlay(app, st, "shutdown-prompt", payload);
            }
        }
    }
}

pub fn snooze(app: &AppHandle, st: &mut St, snoozes_left: u32) {
    st.snoozes = snoozes_left;
    win::destroy(app, st, "shutdown-prompt");
    st.sleep_events.push((wall() + SNOOZE_MS, Ev::Prompt { fresh: false }));
}

pub fn shutdown(app: &AppHandle, st: &mut St) {
    win::destroy(app, st, "shutdown-prompt");
    if crate::data::is_test_dir() {
        eprintln!("test mode: shutdown skipped");
        return;
    }
    if let Err(err) = crate::cmd("shutdown").args(["/s", "/t", "0"]).spawn() {
        eprintln!("failed to execute shutdown: {err}");
    }
}
