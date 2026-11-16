<h1 align="center">StandBuddy</h1>
<p align="center">
  A Windows tray app that makes you stand up every half hour, sends you to bed on time and caps your daily gaming.
</p>
<p align="center">
  <a href="https://github.com/bgtvdm/StandBuddy/releases/latest"><b>Download</b></a> ·
  <a href="#install">Install</a> ·
  <a href="#how-it-works">How it works</a>
</p>
<p align="center">
  <a href="https://github.com/bgtvdm/StandBuddy/releases/latest"><img src="https://img.shields.io/github/v/release/bgtvdm/StandBuddy?style=flat-square&color=c9a66b" alt="Latest release"></a>
  <img src="https://img.shields.io/badge/platform-Windows-3b3431?style=flat-square" alt="Windows">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-3b3431?style=flat-square" alt="MIT license"></a>
</p>

<img src="docs/images/break.webp" alt="Full-screen break overlay: Time to Stand!, a 0:59 countdown, an Emergency Unlock button and a movement tip">

When a break is due, StandBuddy covers the whole screen until the countdown ends. No snooze button, no "remind me later". The only way out early is holding Emergency Unlock for five seconds, and that gets counted. You can switch that exit off in Settings.

- **Breaks that happen.** Every 30 minutes by default, 60 seconds of standing, a short movement tip each time.
- **A bedtime that holds.** Warnings at 30, 15 and 5 minutes, then a shutdown prompt with three snoozes at most.
- **A gaming budget.** One daily limit shared across all your games. When it runs out, the game closes.
- **Private.** No account, no cloud. Stats live in a JSON file on your machine.

### Stats: streaks, the last 7 days and 16 achievements

<p align="center">
  <img src="docs/images/stats.webp" width="420" alt="Stats window: a 12-day streak, 9 breaks completed today, bars for the last 7 days and unlocked achievements">
</p>

### Settings: look, interval, break length, sleep mode, games

<p align="center">
  <img src="docs/images/settings.webp" width="380" alt="Settings window: dark and light mode, six colors, reminder interval, break length, the emergency unlock switch and sleep mode with a 23:00 bedtime">
</p>

### Game limit: the budget is spent

<img src="docs/images/game-blocked.webp" alt="Full-screen notice: Game Limit Reached, you've reached your 3h daily game limit, closing in 4 seconds">

## Install

1. Download `StandBuddy_<version>_x64-setup.exe` from the [latest release](https://github.com/bgtvdm/StandBuddy/releases/latest).
2. Run it. If SmartScreen shows "Unknown publisher", click **More info** → **Run anyway** (the app isn't code-signed).
3. The installer needs no admin rights. It adds Start menu and desktop shortcuts and starts StandBuddy in the tray.
4. Right-click the tray icon → **Settings** to set the interval and turn on **Launch at startup**.

To update, run the newer installer. Settings and stats live in `%APPDATA%\standbuddy` and stay. To remove the app, use **Settings → Apps → Installed apps → StandBuddy → Uninstall**.

Coming from 1.x (the zip version)? Quit it from the tray, run the installer, then delete the old unzipped folder. The installer turns off the old version's autostart for you.

## How it works

- **Breaks.** A timer runs while you work. When it fires, a full-screen overlay stays on top of every window until the break ends. Right-click the tray icon to pause reminders.
- **Sleep mode.** Set a bedtime. StandBuddy warns you ahead of it, then asks to shut the computer down. You can snooze up to three times.
- **Game limiter.** Add games with a file picker or pick them from your Steam library. Play time adds up across all of them. Corner warnings show at 30 and 5 minutes left, and at zero the game process is closed, anti-cheat titles included. You can also pause break reminders while a game runs. The only way around the limit is turning it off in Settings.
- **Themes.** Six colors (Warm, Ocean, Forest, Lavender, Slate, Rose), each in dark and light.

## License

MIT — see [LICENSE](LICENSE).
