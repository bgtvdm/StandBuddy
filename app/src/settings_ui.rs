//! The Settings window's Rust side: fill the form from settings.json, run the file
//! picker and Steam scan off the UI thread, and turn the form back into the JSON
//! `settings_save` takes (same keys and clamps as v2's settings.html).

use crate::ui::{GameRow, ScanRow, Settings, Theme};
use serde_json::{json, Map, Value};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

/// Theme.palette order (ui/theme.slint, bg::palette_index).
const PALETTES: [&str; 6] = ["warm", "ocean", "forest", "lavender", "slate", "rose"];

/// "HH:MM", 24-hour.
pub fn valid_time(text: &str) -> bool {
    let Some((h, m)) = text.trim().split_once(':') else { return false };
    let two_digits = |s: &str| s.len() == 2 && s.bytes().all(|b| b.is_ascii_digit());
    two_digits(h) && two_digits(m) && h.parse::<u32>().is_ok_and(|h| h < 24) && m.parse::<u32>().is_ok_and(|m| m < 60)
}

/// A number as the form shows it: "35", "1.5".
fn number(value: f64) -> String {
    if value.fract() == 0.0 { format!("{}", value as i64) } else { format!("{value}") }
}

/// Add an exe (deduped, case-insensitive); a friendly name replaces the shown name.
fn add_game(win: &Settings, exe: &str, name: Option<&str>) {
    let lower = exe.to_lowercase();
    if lower.is_empty() {
        return;
    }
    let model = win.get_games();
    let Some(rows) = model.as_any().downcast_ref::<VecModel<GameRow>>() else { return };
    if let Some(i) = rows.iter().position(|g| g.exe.to_lowercase() == lower) {
        if let Some(name) = name {
            let mut row = rows.row_data(i).unwrap_or_default();
            row.name = name.into();
            rows.set_row_data(i, row);
        }
        return;
    }
    rows.push(GameRow { exe: lower.clone().into(), name: name.unwrap_or(&lower).into(), pause: false });
}

fn close_scan(win: &Settings) {
    win.set_scan_open(false);
    win.set_scan_status("".into());
    win.set_scan_rows(ModelRc::new(VecModel::<ScanRow>::default()));
}

/// Native "open .exe" dialog owned by `owner` (an HWND as isize). Blocking: call off-thread.
fn pick_exe(owner: isize) -> Option<String> {
    use windows_sys::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    let filter: Vec<u16> = "Executables\0*.exe\0\0".encode_utf16().collect();
    let title: Vec<u16> = "Select a game executable\0".encode_utf16().collect();
    let mut file = [0u16; 1024];
    let mut ofn: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    ofn.hwndOwner = owner as _;
    ofn.lpstrFilter = filter.as_ptr();
    ofn.lpstrFile = file.as_mut_ptr();
    ofn.nMaxFile = file.len() as u32;
    ofn.lpstrTitle = title.as_ptr();
    ofn.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        if GetOpenFileNameW(&mut ofn) == 0 {
            return None;
        }
    }
    let len = file.iter().position(|c| *c == 0).unwrap_or(file.len());
    let path = std::path::PathBuf::from(String::from_utf16_lossy(&file[..len]));
    Some(path.file_name()?.to_string_lossy().to_lowercase())
}

/// Fill the form and wire every callback. `owner` finds this window's HWND for the dialog.
pub fn setup(win: &Settings, payload: &Value, owner: fn() -> isize) {
    let s = payload;
    let num = |key: &str, default: f64| s[key].as_f64().unwrap_or(default);
    win.set_interval_text(number(num("interval", 30.0)).into());
    win.set_break_text(number(num("breakDuration", 60.0)).into());
    win.set_hours_text(number(Some(num("gameLimitHours", 4.0)).filter(|h| *h > 0.0).unwrap_or(4.0)).into());
    win.set_bedtime_text(s["sleepModeBedtime"].as_str().unwrap_or("22:00").into());
    win.set_emergency(s["emergencyUnlock"].as_bool() != Some(false));
    win.set_sleep_on(s["sleepModeEnabled"].as_bool().unwrap_or(false));
    win.set_limiter_on(s["gameLimiterEnabled"].as_bool().unwrap_or(false));
    win.set_launch(s["launchAtLogin"].as_bool().unwrap_or(false));

    let names = s["gameNames"].as_object().cloned().unwrap_or_default();
    let paused: Vec<String> = s["gamePauseList"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| e.as_str().map(str::to_lowercase))
        .collect();
    let rows: Vec<GameRow> = s["gameList"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|exe| {
            let lower = exe.to_lowercase();
            let name = names.get(&lower).and_then(Value::as_str).unwrap_or(exe);
            GameRow { exe: exe.into(), name: name.into(), pause: paused.contains(&lower) }
        })
        .collect();
    win.set_games(ModelRc::new(VecModel::from(rows)));
    win.set_scan_rows(ModelRc::new(VecModel::<ScanRow>::default()));

    win.on_valid_time(|text| valid_time(&text));
    let weak = win.as_weak();
    win.on_remove_game(move |i| {
        if let Some(w) = weak.upgrade() {
            if let Some(rows) = w.get_games().as_any().downcast_ref::<VecModel<GameRow>>() {
                if (i as usize) < rows.row_count() {
                    rows.remove(i as usize);
                }
            }
        }
    });

    let weak = win.as_weak();
    win.on_add_game(move || {
        let (weak, owner) = (weak.clone(), owner());
        std::thread::spawn(move || {
            if let Some(exe) = pick_exe(owner) {
                let _ = weak.upgrade_in_event_loop(move |w| add_game(&w, &exe, None));
            }
        });
    });

    let weak = win.as_weak();
    win.on_scan_steam(move || {
        let Some(w) = weak.upgrade() else { return };
        w.set_scan_open(true);
        w.set_scanning(true);
        w.set_scan_rows(ModelRc::new(VecModel::<ScanRow>::default()));
        w.set_scan_status("Scanning Steam libraries…".into());
        let weak = weak.clone();
        std::thread::spawn(move || {
            let games: Vec<(String, String)> =
                crate::steam::list_games().into_iter().map(|g| (g.name, g.exe)).collect();
            let _ = weak.upgrade_in_event_loop(move |w| {
                w.set_scanning(false);
                if games.is_empty() {
                    w.set_scan_status("No Steam games found. Use \"Add game…\" to pick an .exe manually.".into());
                    return;
                }
                let n = games.len();
                w.set_scan_status(format!("Found {n} game{}. Tick the ones to limit.", if n == 1 { "" } else { "s" }).into());
                let listed: Vec<String> = w.get_games().iter().map(|g| g.exe.to_lowercase()).collect();
                let rows: Vec<ScanRow> = games
                    .into_iter()
                    .map(|(name, exe)| ScanRow {
                        added: listed.contains(&exe.to_lowercase()),
                        name: name.into(),
                        exe: exe.into(),
                        checked: false,
                    })
                    .collect();
                w.set_scan_rows(ModelRc::new(VecModel::from(rows)));
            });
        });
    });

    let weak = win.as_weak();
    win.on_add_selected(move || {
        let Some(w) = weak.upgrade() else { return };
        let picked: Vec<ScanRow> = w.get_scan_rows().iter().filter(|r| r.checked && !r.added).collect();
        for row in picked {
            add_game(&w, &row.exe, Some(&row.name));
        }
        close_scan(&w);
    });
    let weak = win.as_weak();
    win.on_close_scan(move || {
        if let Some(w) = weak.upgrade() {
            close_scan(&w);
        }
    });
}

/// The form as the JSON `settings_save` takes (v2 settings.html `saveSettings`).
pub fn form_json(win: &Settings, previous: &Value) -> Value {
    let theme = win.global::<Theme>();
    let parse = |text: slint::SharedString| text.trim().parse::<f64>().ok();
    let games: Vec<GameRow> = win.get_games().iter().collect();
    let mut names = Map::new();
    for g in &games {
        if g.name != g.exe {
            names.insert(g.exe.to_lowercase(), json!(g.name.as_str()));
        }
    }
    let bedtime = win.get_bedtime_text();
    let bedtime = if valid_time(&bedtime) {
        bedtime.trim().to_string()
    } else {
        previous["sleepModeBedtime"].as_str().unwrap_or("22:00").to_string()
    };
    json!({
        "interval": parse(win.get_interval_text()).unwrap_or(30.0).floor().max(1.0),
        "breakDuration": parse(win.get_break_text()).unwrap_or(60.0).floor().max(10.0),
        "launchAtLogin": win.get_launch(),
        "emergencyUnlock": win.get_emergency(),
        "sleepModeEnabled": win.get_sleep_on(),
        "sleepModeBedtime": bedtime,
        "theme": PALETTES[theme.get_palette().clamp(0, 5) as usize],
        "dark": theme.get_dark(),
        "gameLimiterEnabled": win.get_limiter_on(),
        "gameLimitHours": parse(win.get_hours_text()).filter(|h| *h > 0.0).unwrap_or(4.0).max(0.5),
        "gameList": games.iter().map(|g| g.exe.as_str()).collect::<Vec<_>>(),
        "gameNames": names,
        "gamePauseList": games.iter().filter(|g| g.pause).map(|g| g.exe.to_lowercase()).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::{number, valid_time};

    #[test]
    fn bedtime_must_be_24_hour_hh_mm() {
        assert!(valid_time("22:00"));
        assert!(valid_time("07:30"));
        assert!(!valid_time("7:30"));
        assert!(!valid_time("24:00"));
        assert!(!valid_time("22:60"));
        assert!(!valid_time("10 PM"));
    }

    #[test]
    fn numbers_show_like_the_v2_form() {
        assert_eq!(number(35.0), "35");
        assert_eq!(number(1.5), "1.5");
    }
}
