//! Finds installed Steam games and guesses each one's main executable, so the Game
//! Limiter can offer a list instead of a manual .exe pick. Returns [] on any failure.

use serde::Serialize;
use std::{collections::HashSet, fs, path::{Path, PathBuf}};

#[derive(Serialize)]
pub struct SteamGame {
    pub name: String,
    pub exe: String, // lowercase "game.exe"
    appid: String,
}

struct Exe {
    base: String,
    depth: usize,
    size: u64,
}

// Executable names that are almost never the game itself.
const EXE_BLOCKLIST: &[&str] = &[
    "unitycrashhandler", "unityplayer", "crashreport", "crashpad", "crashhandler",
    "vcredist", "dxsetup", "dxwebsetup", "directx", "dotnet", "dotnetfx",
    "redist", "vc_redist", "oalinst", "eac", "easyanticheat", "battleye",
    "be_setup", "setup", "unins", "uninstall", "installer", "install",
    "cleanup", "config", "configurator", "settings", "touchup", "notification",
    "helper", "service", "updater", "update", "patcher", "report", "reporter",
    "subprocess", "crashsender", "werfault",
];

// Steam "apps" that aren't games: redistributables, Linux runtimes, Proton.
const APPID_BLOCKLIST: [&str; 5] = ["228980", "1070560", "1391110", "1628350", "1493710"];
const NAME_BLOCKLIST: [&str; 4] = ["redistributable", "linux runtime", "steamvr", "steam controller"];

fn steam_path() -> Option<PathBuf> {
    let from_registry = crate::cmd("reg")
        .args(["query", r"HKCU\Software\Valve\Steam", "/v", "SteamPath"])
        .output()
        .ok()
        .and_then(|out| {
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            let line = text.lines().find(|l| l.contains("SteamPath"))?;
            let value = line.split_once("REG_SZ")?.1.trim().replace('/', "\\");
            Some(PathBuf::from(value))
        });
    from_registry
        .into_iter()
        .chain([r"C:\Program Files (x86)\Steam".into(), r"C:\Program Files\Steam".into()])
        .find(|p| p.join("steamapps").exists())
}

/// Every `"key" "value"` pair in a VDF body, ignoring its nesting.
fn vdf_pairs(text: &str) -> Vec<(&str, &str)> {
    let mut pairs = vec![];
    let mut pos = 0;
    while let Some(open) = text[pos..].find('"').map(|i| i + pos) {
        pos = open + 1;
        let Some(close) = text[pos..].find('"').map(|i| i + pos) else { break };
        let after = text[close + 1..].trim_start();
        if close == pos || !after.starts_with('"') {
            continue; // not a key followed by a quoted value; retry from the next quote
        }
        let value_start = text.len() - after.len() + 1;
        let Some(value_end) = text[value_start..].find('"').map(|i| i + value_start) else { break };
        pairs.push((&text[pos..close], &text[value_start..value_end]));
        pos = value_end + 1;
    }
    pairs
}

fn library_folders(steam: &Path) -> Vec<PathBuf> {
    let mut libs = vec![steam.to_path_buf()];
    if let Ok(text) = fs::read_to_string(steam.join("steamapps").join("libraryfolders.vdf")) {
        for (key, value) in vdf_pairs(&text) {
            let lib = PathBuf::from(value.replace("\\\\", "\\"));
            if key.eq_ignore_ascii_case("path") && !value.is_empty() && !libs.contains(&lib) {
                libs.push(lib);
            }
        }
    }
    libs
}

/// .exe files under a game folder, bounded in depth and count.
fn collect_exes(dir: &Path, depth: usize, found: &mut Vec<Exe>) {
    const MAX_DEPTH: usize = 4;
    const MAX_FILES: usize = 600;
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if found.len() >= MAX_FILES {
            return;
        }
        let Ok(kind) = entry.file_type() else { continue };
        let base = entry.file_name().to_string_lossy().into_owned();
        if kind.is_dir() {
            if depth < MAX_DEPTH {
                collect_exes(&entry.path(), depth + 1, found);
            }
        } else if kind.is_file() && base.to_lowercase().ends_with(".exe") {
            let size = entry.metadata().map_or(0, |m| m.len());
            found.push(Exe { base, depth, size });
        }
    }
}

/// Lowercase alphanumerics only — for fuzzy name comparison.
fn fuzzy(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_lowercase()
}

fn pick_best_exe<'a>(exes: &'a [Exe], game_name: &str, install_dir: &str) -> Option<&'a Exe> {
    let targets = [fuzzy(game_name), fuzzy(install_dir)];
    let mut best: Option<(&Exe, f64)> = None;
    for exe in exes {
        let lower = exe.base.to_lowercase();
        let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
        if EXE_BLOCKLIST.iter().any(|blocked| stem.contains(blocked)) {
            continue;
        }
        let name = fuzzy(stem);
        let mut score = 0.0;
        for target in targets.iter().filter(|t| !t.is_empty() && !name.is_empty()) {
            if name == *target {
                score += 100.0;
            } else if target.contains(&name) || name.contains(target.as_str()) {
                score += 50.0;
            }
        }
        score += (exe.size as f64 / (1024.0 * 1024.0)).min(60.0); // bigger = likelier the game
        score -= exe.depth as f64 * 8.0; // prefer the install root
        if best.is_none_or(|(_, top)| score > top) {
            best = Some((exe, score));
        }
    }
    // Everything was blocklisted: take the largest exe outright.
    best.map(|(exe, _)| exe).or_else(|| exes.iter().max_by_key(|e| e.size))
}

pub fn list_games() -> Vec<SteamGame> {
    let Some(steam) = steam_path() else { return vec![] };
    let mut seen = HashSet::new();
    let mut games = vec![];
    for lib in library_folders(&steam) {
        let apps = lib.join("steamapps");
        let Ok(files) = fs::read_dir(&apps) else { continue };
        for file in files.flatten() {
            let file_name = file.file_name().to_string_lossy().to_lowercase();
            if !file_name.starts_with("appmanifest_") || !file_name.ends_with(".acf") {
                continue;
            }
            let Ok(text) = fs::read_to_string(file.path()) else { continue };
            let field = |wanted: &str| {
                // Last match wins, as in the original parser.
                vdf_pairs(&text).into_iter().rev().find(|(k, _)| k.eq_ignore_ascii_case(wanted)).map(|(_, v)| v)
            };
            let (Some(name), Some(install_dir)) = (field("name"), field("installdir")) else { continue };
            let appid = field("appid").unwrap_or("");
            let lower_name = name.to_lowercase();
            if name.is_empty()
                || install_dir.is_empty()
                || APPID_BLOCKLIST.contains(&appid)
                || NAME_BLOCKLIST.iter().any(|b| lower_name.contains(b))
                || lower_name.split(|c: char| !c.is_alphanumeric()).any(|word| word == "proton")
                || (!appid.is_empty() && !seen.insert(appid.to_string()))
            {
                continue;
            }
            let mut exes = vec![];
            collect_exes(&apps.join("common").join(install_dir), 0, &mut exes);
            if let Some(best) = pick_best_exe(&exes, name, install_dir) {
                games.push(SteamGame {
                    name: name.to_string(),
                    exe: best.base.to_lowercase(),
                    appid: appid.to_string(),
                });
            }
        }
    }
    games.sort_by_key(|g| g.name.to_lowercase());
    games
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vdf_pairs_skips_section_names_and_reads_values() {
        let text = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\Steam\"\n\t\t\"label\"\t\t\"\"\n\t}\n}";
        assert_eq!(vdf_pairs(text), vec![("path", "D:\\\\Steam"), ("label", "")]);
    }

    #[test]
    fn picks_the_game_over_helpers_and_crash_handlers() {
        let exe = |base: &str, depth, mb: u64| Exe { base: base.into(), depth, size: mb << 20 };
        let exes = [exe("UnityCrashHandler64.exe", 0, 90), exe("launcher_helper.exe", 0, 80), exe("Hades.exe", 1, 20)];
        assert_eq!(pick_best_exe(&exes, "Hades", "Hades").unwrap().base, "Hades.exe");
    }
}
