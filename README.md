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

When a break is due, StandBuddy covers the whole screen until the countdown ends. No snooze button, no "remind me later". The only way out early is holding Emergency Unlock for five seconds, and that gets counted.

- **Breaks that happen.** Every 30 minutes by default, 60 seconds of standing, a short movement tip each time.
- **A bedtime that holds.** Warnings at 30, 15 and 5 minutes, then a shutdown prompt with three snoozes at most.
- **A gaming budget.** One daily limit shared across all your games. When it runs out, the game closes.
- **Private.** No account, no cloud. Stats live in a JSON file on your machine.

### Stats: streaks, the last 7 days and 16 achievements

<p align="center">
  <img src="docs/images/stats.webp" width="420" alt="Stats window: 9 breaks completed today, a 12-day streak and bars for the last 7 days">
</p>

### Settings: interval, break length, sleep mode, games

<p align="center">
  <img src="docs/images/settings.webp" width="380" alt="Settings window: six color themes, reminder interval, break duration and sleep mode with a 23:00 bedtime">
</p>

### Game limit: the budget is spent

<img src="docs/images/game-blocked.webp" alt="Full-screen notice: Game Limit Reached, you've reached your 3h daily game limit, closing in 4 seconds">

## Install

StandBuddy ships as a Windows zip, there is no installer.

### With an AI agent

Paste this into Claude Code, Codex, Hermes or any agent that can run commands on your computer:

```text
Install StandBuddy from https://github.com/bgtvdm/StandBuddy.
1. Download the StandBuddy-*-win.zip asset of the latest release
   (https://api.github.com/repos/bgtvdm/StandBuddy/releases/latest).
2. Quit StandBuddy if it is running (tray icon → Quit).
3. Extract the zip into %LOCALAPPDATA%\Programs\StandBuddy, overwriting files
   in place if it is already there. Settings and stats live elsewhere and stay.
4. Start StandBuddy.exe from that folder and check that the tray icon appears.
5. If Windows SmartScreen blocks it, tell me to click "More info" → "Run anyway".
```

SmartScreen asks once because the app isn't code-signed. That click is yours.

### By hand

1. Download `StandBuddy-<version>-win.zip` from the [latest release](https://github.com/bgtvdm/StandBuddy/releases/latest).
2. Extract it to a folder you'll keep, for example `%LOCALAPPDATA%\Programs\StandBuddy`.
3. Run `StandBuddy.exe`. If SmartScreen shows "Unknown publisher", click **More info** → **Run anyway**.
4. Right-click the tray icon → **Settings** to set the interval and turn on **Launch at startup**.

To update, quit StandBuddy, extract the new zip over the same folder and start it again.

## How it works

- **Breaks.** A timer runs while you work. When it fires, a full-screen overlay stays on top of every window until the break ends. Right-click the tray icon to pause reminders.
- **Sleep mode.** Set a bedtime. StandBuddy warns you ahead of it, then asks to shut the computer down. You can snooze up to three times.
- **Game limiter.** Add games with a file picker or pick them from your Steam library. Play time adds up across all of them. Corner warnings show at 30 and 5 minutes left, and at zero the game process is closed, anti-cheat titles included. You can also pause break reminders while a game runs. The only way around the limit is turning it off in Settings.
- **Themes.** Six palettes: Warm, Ocean, Forest, Lavender, Slate, Rose.

## License

MIT — see [LICENSE](LICENSE).
