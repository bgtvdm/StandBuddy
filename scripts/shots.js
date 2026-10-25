// Regenerates the README screenshots in docs/images/ from the real windows with
// synthetic data: npm run shots
const { app, BrowserWindow } = require('electron');
const fs = require('fs');
const path = require('path');

// shortcut: 1.5x, not 2x: Windows clamps a window to the work area, and 900px at 2x
// needs a 1800px-tall screen. Raise to 2 when shooting on a taller display.
const SCALE = 1.5;
app.commandLine.appendSwitch('force-device-scale-factor', String(SCALE));

const root = path.join(__dirname, '..');
const out = path.join(root, 'docs', 'images');
const tips = JSON.parse(fs.readFileSync(path.join(root, 'tips.json'), 'utf8'));

function lastWeek(counts) {
  return counts.map((completed, i) => {
    const d = new Date();
    d.setDate(d.getDate() - (counts.length - 1 - i));
    return {
      date: d.toISOString().slice(0, 10),
      dayName: d.toLocaleDateString('en-US', { weekday: 'short' }),
      completed
    };
  });
}

const unlocked = ['first_break', 'daily_5', 'daily_10', 'streak_3', 'streak_7', 'total_25', 'total_100'];
const achievementNames = {
  first_break: ['First Steps', 'Complete your first break', '🎯'],
  daily_5: ['Getting Started', '5 breaks in a day', '⭐'],
  daily_10: ['Dedicated', '10 breaks in a day', '🌟'],
  daily_15: ['Health Champion', '15 breaks in a day', '🏆'],
  streak_3: ['Building Habits', '3-day streak', '🔥'],
  streak_7: ['One Week Strong', '7-day streak', '💪'],
  streak_14: ['Two Week Warrior', '14-day streak', '⚡'],
  total_25: ['Quarter Century', '25 total breaks', '🌱'],
  total_100: ['Century Club', '100 total breaks', '💯'],
  total_300: ['Consistent', '300 total breaks', '🎖️']
};

const stats = {
  theme: 'warm',
  today: { completed: 9, emergency: 0, shown: 10, breakMinutes: 9 },
  streak: 12,
  totalCompleted: 137,
  last7Days: lastWeek([8, 11, 6, 10, 12, 4, 9]),
  avgMinutesBetween: 30,
  achievements: Object.entries(achievementNames).map(([id, [name, description, icon]]) => ({
    id, name, description, icon,
    unlocked: unlocked.includes(id),
    unlockedAt: unlocked.includes(id) ? new Date().toISOString() : null
  }))
};

const settings = {
  interval: 30, breakDuration: 60, nextTipIndex: 0, launchAtLogin: true,
  todayDate: '', todayBreaks: 9, activeDays: [],
  sleepModeEnabled: true, sleepModeBedtime: '23:00', theme: 'warm',
  gameLimiterEnabled: true, gameLimitHours: 3,
  gameList: ['cs2.exe', 'dota2.exe', 'eldenring.exe'],
  gameNames: { 'cs2.exe': 'Counter-Strike 2', 'dota2.exe': 'Dota 2', 'eldenring.exe': 'ELDEN RING' },
  gamePauseList: ['cs2.exe']
};

const shots = [
  { name: 'break', file: 'break.html', w: 1280, h: 900, title: 'StandBuddy', channel: 'break-start',
    data: { duration: 60, tip: tips[0], theme: 'warm' }, wait: 2500 },
  { name: 'stats', file: 'stats.html', w: 480, h: 640, title: 'Stats', channel: 'stats-load', data: stats },
  { name: 'settings', file: 'settings.html', w: 380, h: 760, title: 'Settings', channel: 'settings-load', data: settings },
  { name: 'game-blocked', file: 'game-blocked.html', w: 1280, h: 900, title: 'StandBuddy', channel: 'game-blocked-start',
    data: { theme: 'warm', limitHours: 3 } }
];

// Bakes a window bar, 12px corners and a soft shadow on transparent padding into a WebP.
function frameScript(png, title) {
  return `(async () => {
    const img = new Image(); img.src = ${JSON.stringify(png)}; await img.decode();
    const s = ${SCALE}, pad = 40 * s, bar = 34 * s, r = 12 * s;
    const c = document.createElement('canvas');
    c.width = img.width + pad * 2; c.height = img.height + bar + pad * 2;
    const x = c.getContext('2d');
    const shape = () => { x.beginPath(); x.roundRect(pad, pad, img.width, img.height + bar, r); };
    x.save(); x.shadowColor = 'rgba(0,0,0,0.22)'; x.shadowBlur = 28 * s; x.shadowOffsetY = 10 * s;
    shape(); x.fillStyle = '#fff'; x.fill(); x.restore();
    x.save(); shape(); x.clip();
    x.fillStyle = '#ebe7e2'; x.fillRect(pad, pad, img.width, bar);
    x.drawImage(img, pad, pad + bar); x.restore();
    ['#ff5f57', '#febc2e', '#28c840'].forEach((col, i) => {
      x.beginPath(); x.arc(pad + (18 + i * 20) * s, pad + bar / 2, 6 * s, 0, Math.PI * 2);
      x.fillStyle = col; x.fill();
    });
    x.font = (12.5 * s) + 'px "Segoe UI", sans-serif'; x.fillStyle = '#6b625a';
    x.textAlign = 'center'; x.textBaseline = 'middle';
    x.fillText(${JSON.stringify(title)}, pad + img.width / 2, pad + bar / 2);
    return c.toDataURL('image/webp', 0.9);
  })()`;
}

app.whenReady().then(async () => {
  fs.mkdirSync(out, { recursive: true });
  const framer = new BrowserWindow({ show: false });
  await framer.loadURL('about:blank');

  for (const shot of shots) {
    const win = new BrowserWindow({
      width: shot.w, height: shot.h, show: false, frame: false, useContentSize: true,
      webPreferences: { preload: path.join(root, 'preload.js'), contextIsolation: true }
    });
    win.webContents.once('dom-ready', () => win.webContents.send(shot.channel, shot.data));
    await win.loadFile(path.join(root, shot.file));
    await win.webContents.insertCSS('::-webkit-scrollbar { display: none; }');
    await new Promise((resolve) => setTimeout(resolve, shot.wait || 1200));
    const png = (await win.webContents.capturePage()).toDataURL();
    const webp = await framer.webContents.executeJavaScript(frameScript(png, shot.title));
    const file = path.join(out, `${shot.name}.webp`);
    fs.writeFileSync(file, Buffer.from(webp.split(',')[1], 'base64'));
    console.log(`${shot.name}.webp  ${Math.round(fs.statSync(file).size / 1024)} KB`);
    win.destroy();
  }
  app.quit();
}).catch((err) => { console.error(err); app.exit(1); });
