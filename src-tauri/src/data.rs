//! settings.json / stats.json — same files and shapes the Electron build wrote.

use chrono::{Days, Local, SecondsFormat, Utc};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::{collections::BTreeMap, fs, path::PathBuf};

/// Set to run against a scratch folder; also disables shutdown, autostart and
/// kill-task registration so a test run can't touch the real machine state.
pub fn is_test_dir() -> bool {
    std::env::var_os("STANDBUDDY_DATA_DIR").is_some()
}

/// The Electron build's userData folder, so settings, stats and the registered
/// kill task carry over unchanged.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("STANDBUDDY_DATA_DIR") {
        return dir.into();
    }
    PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("standbuddy")
}

pub fn today() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub interval: f64,       // minutes between breaks
    pub break_duration: f64, // seconds
    pub next_tip_index: usize,
    pub launch_at_login: bool,
    pub today_date: String,
    pub today_breaks: u32,
    pub active_days: Vec<String>,
    pub sleep_mode_enabled: bool,
    pub sleep_mode_bedtime: String,
    pub theme: String,
    pub emergency_unlock: bool, // the break overlay offers the 5-second-hold exit
    pub dark: bool, // Settings/Stats windows: dark or light variant of the theme
    pub game_limiter_enabled: bool,
    pub game_limit_hours: f64,
    pub game_list: Vec<String>,
    pub game_names: Map<String, Value>,
    pub game_pause_list: Vec<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>, // unknown keys survive a round trip
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            interval: 30.0,
            break_duration: 60.0,
            next_tip_index: 0,
            launch_at_login: false,
            today_date: String::new(),
            today_breaks: 0,
            active_days: vec![],
            sleep_mode_enabled: false,
            sleep_mode_bedtime: "22:00".into(),
            theme: "warm".into(),
            emergency_unlock: true,
            dark: true,
            game_limiter_enabled: false,
            game_limit_hours: 4.0,
            game_list: vec![],
            game_names: Map::new(),
            game_pause_list: vec![],
            extra: Map::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct DayStats {
    pub completed: u32,
    pub emergency: u32,
    pub shown: u32,
    pub break_minutes: f64,
    pub game_minutes: f64,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Stats {
    pub days: BTreeMap<String, DayStats>,
    pub used_days: Vec<String>,
    pub total_completed: u32,
    pub achievements: Map<String, Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

pub fn load<T: Default + DeserializeOwned>(file: &str) -> T {
    let path = data_dir().join(file);
    let Ok(text) = fs::read_to_string(&path) else { return T::default() };
    serde_json::from_str(&text).unwrap_or_else(|err| {
        // Defaults will overwrite this file on the next save — keep the original.
        let _ = fs::copy(&path, path.with_extension("json.bad"));
        eprintln!("{file} unreadable ({err}); original kept as .json.bad");
        T::default()
    })
}

pub fn save<T: Serialize>(file: &str, value: &T) {
    let dir = data_dir();
    let result = fs::create_dir_all(&dir)
        .and_then(|_| fs::write(dir.join(file), serde_json::to_string_pretty(value).unwrap()));
    if let Err(err) = result {
        eprintln!("failed to save {file}: {err}");
    }
}

// (id, name, description, icon, metric, threshold) — metric: t = total breaks,
// d = breaks today, s = streak days.
const ACHIEVEMENTS: [(&str, &str, &str, &str, char, u32); 16] = [
    ("first_break", "First Steps", "Complete your first break", "🎯", 't', 1),
    ("daily_5", "Getting Started", "5 breaks in a day", "⭐", 'd', 5),
    ("daily_10", "Dedicated", "10 breaks in a day", "🌟", 'd', 10),
    ("daily_15", "Health Champion", "15 breaks in a day", "🏆", 'd', 15),
    ("daily_20", "Unstoppable", "20 breaks in a day", "🚀", 'd', 20),
    ("streak_3", "Building Habits", "3-day streak", "🔥", 's', 3),
    ("streak_7", "One Week Strong", "7-day streak", "💪", 's', 7),
    ("streak_14", "Two Week Warrior", "14-day streak", "⚡", 's', 14),
    ("streak_30", "Monthly Master", "30-day streak", "👑", 's', 30),
    ("streak_60", "Iron Will", "60-day streak", "💎", 's', 60),
    ("total_25", "Quarter Century", "25 total breaks", "🌱", 't', 25),
    ("total_100", "Century Club", "100 total breaks", "💯", 't', 100),
    ("total_300", "Consistent", "300 total breaks", "🎖️", 't', 300),
    ("total_500", "Halfway Hero", "500 total breaks", "🏅", 't', 500),
    ("total_750", "Almost There", "750 total breaks", "🥈", 't', 750),
    ("total_1000", "Legendary", "1000 total breaks", "🥇", 't', 1000),
];

impl Stats {
    pub fn day(&mut self) -> &mut DayStats {
        self.days.entry(today()).or_default()
    }

    pub fn touch_used_day(&mut self) {
        let today = today();
        if !self.used_days.contains(&today) {
            self.used_days.push(today);
        }
    }

    fn completed_on(&self, day: &str) -> u32 {
        self.days.get(day).map_or(0, |d| d.completed)
    }

    /// Consecutive used days (newest first) with at least one completed break.
    /// Days the app wasn't used are simply absent, so they don't break the streak.
    pub fn streak(&self) -> u32 {
        let mut days = self.used_days.clone();
        days.sort();
        days.iter().rev().take_while(|d| self.completed_on(d) >= 1).count() as u32
    }

    pub fn unlock_achievements(&mut self) {
        let (total, today_done, streak) =
            (self.total_completed, self.completed_on(&today()), self.streak());
        for (id, _, _, _, metric, threshold) in ACHIEVEMENTS {
            let value = match metric {
                't' => total,
                'd' => today_done,
                _ => streak,
            };
            if value >= threshold && !self.achievements.contains_key(id) {
                let at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
                self.achievements.insert(id.into(), json!({ "unlockedAt": at }));
            }
        }
    }

    /// Payload for stats.html.
    pub fn for_window(&self, interval: f64) -> Value {
        let now = Local::now().date_naive();
        let last7: Vec<Value> = (0..7u64)
            .rev()
            .map(|back| {
                let d = now - Days::new(back);
                let key = d.format("%Y-%m-%d").to_string();
                json!({
                    "completed": self.completed_on(&key),
                    "dayName": d.format("%a").to_string(),
                    "date": key,
                })
            })
            .collect();
        let any_completed = last7.iter().any(|d| d["completed"] != 0);
        let achievements: Vec<Value> = ACHIEVEMENTS
            .iter()
            .map(|(id, name, description, icon, _, _)| {
                let unlocked = self.achievements.get(*id);
                json!({
                    "id": id, "name": name, "description": description, "icon": icon,
                    "unlocked": unlocked.is_some(),
                    "unlockedAt": unlocked.and_then(|a| a.get("unlockedAt")),
                })
            })
            .collect();
        json!({
            "today": self.days.get(&today()).cloned().unwrap_or_default(),
            "streak": self.streak(),
            "totalCompleted": self.total_completed,
            "last7Days": last7,
            "avgMinutesBetween": if any_completed { interval } else { 0.0 },
            "achievements": achievements,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streak_stops_at_first_used_day_without_a_completed_break() {
        let mut s = Stats::default();
        for (day, completed) in [("2026-01-01", 2), ("2026-01-02", 0), ("2026-01-05", 1), ("2026-01-06", 3)] {
            s.used_days.push(day.into());
            s.days.insert(day.into(), DayStats { completed, ..Default::default() });
        }
        // 01-03/04 unused: skipped. 01-02 used with zero breaks: ends the streak.
        assert_eq!(s.streak(), 2);
    }

    #[test]
    fn electron_settings_round_trip_keeps_unknown_keys() {
        let s: Settings =
            serde_json::from_str(r#"{"interval":35,"theme":"ocean","futureKey":1}"#).unwrap();
        assert_eq!((s.interval, s.theme.as_str(), s.break_duration), (35.0, "ocean", 60.0));
        assert_eq!(serde_json::to_value(&s).unwrap()["futureKey"], 1);
    }
}
