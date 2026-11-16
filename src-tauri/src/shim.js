// Stands in for Electron's preload.js: gives the unchanged HTML windows the same
// window.electronAPI, backed by Tauri commands. Each window pulls its payload once
// the DOM is parsed, renders it, then asks to be shown — so it is never revealed blank.
(function () {
  const inv = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);
  const domReady = new Promise((resolve) => {
    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', resolve);
    else resolve();
  });
  const load = (cb) => {
    Promise.all([inv('get_payload'), domReady]).then(([data]) => {
      cb(data);
      inv('reveal');
    });
  };
  let onReset = null;

  // Frameless Settings/Stats windows: the page header marked [data-drag] moves the window.
  document.addEventListener('mousedown', (e) => {
    if (e.button === 0 && e.target.closest('[data-drag]') && !e.target.closest('button')) inv('drag_self');
  });

  window.electronAPI = {
    onSettingsLoad: load,
    onBreakStart: load,
    onStatsLoad: load,
    onSleepWarningStart: load,
    onShutdownPromptStart: load,
    onGameWarningStart: load,
    onGameBlockedStart: load,
    onStatsReset: (cb) => { onReset = cb; },
    saveSettings: (settings) => inv('settings_save', { newSettings: settings }),
    cancelSettings: () => inv('close_self'),
    closeStats: () => inv('close_self'),
    skipBreak: () => inv('break_skip'),
    resetStats: () => inv('stats_reset').then((data) => onReset && onReset(data)),
    closeSleepWarning: () => inv('sleep_warning_close'),
    snoozeShutdown: (snoozesLeft) => inv('shutdown_snooze', { snoozesLeft }),
    executeShutdown: () => inv('shutdown_execute'),
    pickGameExe: () => inv('game_pick_exe'),
    scanSteamGames: () => inv('steam_list_games')
  };
})();
