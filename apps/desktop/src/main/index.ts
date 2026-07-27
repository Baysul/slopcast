import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import * as native from '@slopcast/native-rust';
import {
  DEFAULT_STREAM_SETTINGS,
  type ResolutionPreset,
  type StreamSettings,
  type VideoCodec,
} from '@slopcast/shared-types';
import { loadConfig } from '@slopcast/shared-types/config';
import { app, BrowserWindow, clipboard, desktopCapturer, ipcMain, Menu, nativeImage, session } from 'electron';

const appConfig = loadConfig();

let mainWindow: BrowserWindow | null = null;
let lastCapturedSourceName: string | null = null;

interface CaptureContext {
  de: 'unknown' | 'kde' | 'gnome';
  mediaName: string | null;
  sourceType: 'monitor' | 'window' | 'unknown';
  videoNodeCount: number;
}

let lastCaptureContext: CaptureContext | null = null;

const isWayland =
  process.platform === 'linux' && (process.env.XDG_SESSION_TYPE === 'wayland' || !!process.env.WAYLAND_DISPLAY);

// ── Stream Settings Persistence ─────────────────────────────────────────
// Stored as JSON in Electron's per-platform user-data directory
// (%APPDATA%/<app-name> on Windows, ~/.config/<app-name> on Linux,
// ~/Library/Application Support/<app-name> on macOS).
const STREAM_SETTINGS_FILE = 'stream-settings.json';
let streamSettingsCache: StreamSettings | null = null;

const streamSettingsPath = (): string => path.join(app.getPath('userData'), STREAM_SETTINGS_FILE);

// Hand-edited or corrupt files must never crash the app — fall back per field.
function sanitizeStreamSettings(raw: unknown): StreamSettings {
  const d = DEFAULT_STREAM_SETTINGS;
  if (typeof raw !== 'object' || raw === null) return d;
  const o = raw as Record<string, unknown>;
  const num = (v: unknown, min: number, max: number, fallback: number): number =>
    typeof v === 'number' && Number.isFinite(v) && v >= min && v <= max ? v : fallback;
  const codec = (v: unknown): VideoCodec =>
    v === 'h264' || v === 'h265' || v === 'vp8' || v === 'vp9' || v === 'av1' ? v : d.videoCodec;
  const resolution = (v: unknown): ResolutionPreset =>
    v === '1080p' || v === '1440p' || v === '2160p' ? v : d.resolution;
  return {
    fps: num(o.fps, 1, 240, d.fps),
    bitrateLimit: num(o.bitrateLimit, 100_000, 200_000_000, d.bitrateLimit),
    videoCodec: codec(o.videoCodec),
    resolution: resolution(o.resolution),
    apiEndpoint: typeof o.apiEndpoint === 'string' && o.apiEndpoint.trim() !== '' ? o.apiEndpoint : d.apiEndpoint,
  };
}

function loadStreamSettings(): StreamSettings {
  if (streamSettingsCache) return streamSettingsCache;
  const file = streamSettingsPath();
  let parsed: unknown = null;
  if (existsSync(file)) {
    try {
      parsed = JSON.parse(readFileSync(file, 'utf-8'));
    } catch (err) {
      console.error(`Failed to parse ${STREAM_SETTINGS_FILE}, using defaults:`, err);
    }
  }
  streamSettingsCache = sanitizeStreamSettings(parsed);
  return streamSettingsCache;
}

function saveStreamSettings(raw: unknown): boolean {
  const settings = sanitizeStreamSettings(raw);
  try {
    writeFileSync(streamSettingsPath(), `${JSON.stringify(settings, null, 2)}\n`, 'utf-8');
    streamSettingsCache = settings;
    return true;
  } catch (err) {
    console.error(`Failed to write ${STREAM_SETTINGS_FILE}:`, err);
    return false;
  }
}

// ── Hardware-Accelerated Video Encoding ─────────────────────────────────
// Flags must be set before app.whenReady(). Build one combined list because
// appendSwitch stores only the *last* value for the same switch name.
const features: string[] = [];

if (isWayland) {
  features.push('WebRTCPipeWireCapturer');
  features.push('WaylandLinuxDrmSyncobj');
}

features.push('PlatformHEVCDecoderSupport');

switch (process.platform) {
  case 'linux':
    features.push('AcceleratedVideoEncoder');
    features.push('AcceleratedVideoDecodeLinuxGL');
    if (isWayland) {
      features.push('AcceleratedVideoDecodeLinuxZeroCopyGL');
    }
    features.push('VaapiIgnoreDriverChecks');
    features.push('CanvasOopRasterization');
    break;
  case 'win32':
    features.push('D3D11VideoEncoder', 'D3D11VideoDecoder');
    break;
}

app.commandLine.appendSwitch('enable-gpu-rasterization');
app.commandLine.appendSwitch('enable-gpu-memory-buffer-video-frames');
app.commandLine.appendSwitch('ignore-gpu-blocklist');
app.commandLine.appendSwitch('enable-features', features.join(','));
app.commandLine.appendSwitch('enable-low-latency-video-decoder');
app.commandLine.appendSwitch('disable-background-timer-throttling');
app.commandLine.appendSwitch('disable-renderer-backgrounding');
app.commandLine.appendSwitch('no-zygote');

if (isWayland) {
  app.commandLine.appendSwitch('use-gl', 'angle');
  app.commandLine.appendSwitch('use-angle', 'vulkan');
}

function resolveIconPath(): string | null {
  const candidates = [
    path.join(app.getAppPath(), 'resources', 'icon.png'),
    path.join(__dirname, '../../resources/icon.png'),
  ];
  return candidates.find((p) => existsSync(p)) ?? null;
}

function toCaptureContext(raw: native.CaptureContext): CaptureContext {
  return {
    de: raw.de === 'kde' || raw.de === 'gnome' ? raw.de : 'unknown',
    sourceType: raw.sourceType === 'monitor' || raw.sourceType === 'window' ? raw.sourceType : 'unknown',
    mediaName: raw.mediaName ?? null,
    videoNodeCount: raw.videoNodeCount,
  };
}

function createWindow() {
  const iconPath = resolveIconPath();
  let icon: Electron.NativeImage | undefined;
  if (iconPath) {
    icon = nativeImage.createFromPath(iconPath);
  }

  mainWindow = new BrowserWindow({
    width: 1100,
    height: 900,
    title: 'Slopcast Desktop Presenter',
    backgroundColor: '#090d16',
    icon,
    webPreferences: {
      nodeIntegration: false,
      contextIsolation: true,
      preload: path.join(__dirname, 'preload.js'),
      backgroundThrottling: false,
    },
  });

  mainWindow.webContents.on('console-message', (_e, _level, message) => {
    console.log(`[renderer] ${message}`);
  });

  Menu.setApplicationMenu(
    Menu.buildFromTemplate([
      {
        label: 'View',
        submenu: [
          { role: 'reload', accelerator: 'CmdOrCtrl+R' },
          { type: 'separator' },
          {
            label: 'Toggle Developer Tools',
            accelerator: 'CmdOrCtrl+Shift+I',
            click: () => mainWindow?.webContents.toggleDevTools(),
          },
          { type: 'separator' },
          {
            label: 'GPU Internals',
            accelerator: 'CmdOrCtrl+Shift+G',
            click: () => {
              const win = new BrowserWindow({ width: 960, height: 800, title: 'chrome://gpu' });
              win.loadURL('chrome://gpu');
            },
          },
          {
            label: 'WebRTC Internals',
            accelerator: 'CmdOrCtrl+Shift+W',
            click: () => {
              const win = new BrowserWindow({ width: 960, height: 800, title: 'chrome://webrtc-internals' });
              win.loadURL('chrome://webrtc-internals');
            },
          },
        ],
      },
    ]),
  );
  mainWindow.autoHideMenuBar = true;
  mainWindow.setMenuBarVisibility(false);
  mainWindow.maximize();

  const devServerUrl = process.env.VITE_DEV_SERVER_URL;
  if (devServerUrl) {
    mainWindow.loadURL(devServerUrl);
  } else {
    mainWindow.loadFile(path.join(__dirname, '../renderer/index.html'));
  }

  mainWindow.on('closed', () => {
    mainWindow = null;
    stopNativeCapture();
  });

  // Windows fires this on logoff/shutdown instead of before-quit — native
  // sessions must be torn down there too.
  mainWindow.on('session-end', () => {
    stopNativeCapture();
  });
}

function stopNativeCapture() {
  try {
    native.stopAudioCapture();
    console.log('🛑 Audio capture stopped');
  } catch (err) {
    console.error('Failed to stop audio capture:', err);
  }
  try {
    native.stopAudioMetering();
  } catch (err) {
    console.error('Failed to stop audio metering:', err);
  }
}

app.whenReady().then(() => {
  app.setName('slopcast');
  console.log('====================================================');
  console.log('🚀 Launching Desktop Presenter Application');
  console.log(`   Platform: ${process.platform} (${isWayland ? 'Wayland - xdg-desktop-portal' : 'X11/native'})`);
  console.log('====================================================');

  // Auto-grant media so the renderer can open the virtual capture mic
  // without an interactive portal prompt after screenshare start.
  session.defaultSession.setPermissionRequestHandler((_wc, permission, callback) => {
    if (permission === 'media' || permission === 'mediaKeySystem') {
      callback(true);
      return;
    }
    callback(false);
  });
  session.defaultSession.setPermissionCheckHandler((_wc, permission) => {
    return permission === 'media' || permission === 'mediaKeySystem';
  });

  try {
    const initMsg = native.initEngine();
    console.log(`[Native Rust] ${initMsg}`);

    const audioApps = native.listAudioApplications();
    console.log(`🔊 Detected ${audioApps.length} active audio applications:`);
    for (const app of audioApps) {
      console.log(`  - [ID: ${app.id}] ${app.name} (Process ID: ${app.processId})`);
    }
  } catch (err) {
    console.error('❌ Native audio engine error:', err);
  }

  session.defaultSession.setDisplayMediaRequestHandler((_request, callback) => {
    desktopCapturer
      .getSources({ types: ['window', 'screen'] })
      .then((sources) => {
        if (sources.length === 0) {
          console.error('desktopCapturer returned no sources');
          callback({});
          return;
        }
        // Prefer a window source: this app shares windows, not full screens.
        const source = sources.find((s) => s.id.startsWith('window')) ?? sources[0];
        lastCapturedSourceName = source.name;
        console.log(`[setDisplayMediaRequestHandler] storing source name="${source.name}" (id=${source.id})`);
        callback({ video: source });
      })
      .catch((err) => {
        console.error('getSources failed:', err);
        callback({});
      });
  });

  // IPC Handlers
  ipcMain.handle('get-app-config', () => ({
    apiEndpoint: appConfig.apiEndpoint,
    livekitUrl: appConfig.livekitUrl,
  }));

  ipcMain.handle('get-stream-settings', () => loadStreamSettings());

  ipcMain.handle('save-stream-settings', (_event, raw: unknown) => saveStreamSettings(raw));

  ipcMain.handle('get-platform-info', () => ({
    platform: process.platform,
    isWayland,
  }));

  ipcMain.handle('get-audio-apps', () => {
    try {
      return native.listAudioApplications();
    } catch (err) {
      console.error('get-audio-apps IPC error:', err);
      return [];
    }
  });

  ipcMain.handle('start-audio-capture', (_event, targetId: number | string) => {
    try {
      // Exclusive capture: ONLY the target application's audio is linked
      // into the virtual capture sink; everything else is never captured.
      return native.startAudioCapture(targetId);
    } catch (err) {
      console.error('start-audio-capture IPC error:', err);
      return false;
    }
  });

  ipcMain.handle('stop-audio-capture', () => {
    try {
      return native.stopAudioCapture();
    } catch (err) {
      console.error('stop-audio-capture IPC error:', err);
      return false;
    }
  });

  ipcMain.handle('switch-audio-capture', (_event, targetId: number | string) => {
    try {
      // Switch the audio target on the fly without destroying the virtual capture
      // node — the existing MediaStreamTrack stays alive and seamless.
      return native.switchAudioCapture(targetId);
    } catch (err) {
      console.error('switch-audio-capture IPC error:', err);
      return false;
    }
  });

  ipcMain.handle('start-audio-metering', () => {
    try {
      return native.startAudioMetering();
    } catch (err) {
      console.error('start-audio-metering IPC error:', err);
      return false;
    }
  });

  ipcMain.handle('stop-audio-metering', () => {
    try {
      return native.stopAudioMetering();
    } catch (err) {
      console.error('stop-audio-metering IPC error:', err);
      return false;
    }
  });

  ipcMain.handle('get-audio-levels', () => {
    try {
      return native.getAudioLevels();
    } catch (err) {
      console.error('get-audio-levels IPC error:', err);
      return [];
    }
  });

  ipcMain.handle('get-desktop-sources', async () => {
    const sources = await desktopCapturer.getSources({ types: ['screen', 'window'] });
    return sources.map((s) => ({
      id: s.id,
      name: s.name,
      thumbnail: s.thumbnail.toDataURL(),
    }));
  });

  const resolveAudioForWayland = async (nameHint: string | undefined): Promise<native.AudioApp | null> => {
    // Layer 1: PipeWire introspection — retry as xdg-desktop-portal may lag.
    for (let attempt = 0; attempt < 3; attempt++) {
      try {
        const app = native.resolveAudioAppForCapturedWindow();
        if (app) {
          console.log(`[resolve-audio-source] Wayland PW-introspect → "${app.name}" (PID ${app.processId})`);
          return app;
        }
      } catch (err) {
        console.error('resolve-audio-source Wayland introspection error:', err);
      }
      if (attempt < 2) {
        await new Promise((resolve) => setTimeout(resolve, 200));
      }
    }

    // Layer 2: Name matching via Rust.
    const hint = nameHint ?? lastCapturedSourceName;
    if (hint) {
      try {
        const app = native.resolveAudioAppByName(hint);
        if (app) {
          console.log(`[resolve-audio-source] Wayland name-match "${hint}" → "${app.name}"`);
          return app;
        }
      } catch (err) {
        console.error('resolve-audio-source Wayland name-match error:', err);
      }
    }

    // Layer 3: native video-graph introspection — reports which desktop
    // environment is streaming, whether the source is a monitor or a window,
    // and the best-matched audio app for the captured source.
    try {
      const ctx = native.getCaptureContext();
      lastCaptureContext = toCaptureContext(ctx);
      console.log(
        `[resolve-audio-source] Wayland context: de=${lastCaptureContext.de} sourceType=${lastCaptureContext.sourceType} mediaName="${lastCaptureContext.mediaName ?? ''}" videoNodes=${lastCaptureContext.videoNodeCount}`,
      );
      if (ctx.app) {
        console.log(`[resolve-audio-source] Wayland context-match → "${ctx.app.name}"`);
        return ctx.app;
      }
    } catch (err) {
      console.error('[resolve-audio-source] capture-context error:', err);
      lastCaptureContext = { de: 'unknown', mediaName: null, sourceType: 'unknown', videoNodeCount: 0 };
    }

    console.log(
      `[resolve-audio-source] Wayland: no match (introspect=null, nameHint="${nameHint ?? ''}", lastSource="${lastCapturedSourceName ?? ''}")`,
    );
    return null;
  };

  const resolveAudioForX11 = (sourceId: string | undefined, nameHint: string | undefined): native.AudioApp | null => {
    // Layer 1: _NET_WM_PID via X11 window ID.
    if (sourceId?.startsWith('window:')) {
      const windowId = parseInt(sourceId.split(':')[1], 10);
      if (!Number.isNaN(windowId)) {
        try {
          const app = native.resolveAudioAppForX11Window(windowId);
          if (app) {
            console.log(`[resolve-audio-source] X11 PID-match: window ${windowId} → "${app.name}"`);
            return app;
          }
        } catch (err) {
          console.error('resolve-audio-source X11 error:', err);
        }
      }
    }

    // Layer 2: Name matching via Rust.
    if (nameHint) {
      try {
        const app = native.resolveAudioAppByName(nameHint);
        if (app) {
          console.log(`[resolve-audio-source] X11 name-match "${nameHint}" → "${app.name}"`);
          return app;
        }
      } catch (err) {
        console.error('resolve-audio-source X11 name-match error:', err);
      }
    }

    console.log(`[resolve-audio-source] X11: no match (sourceId="${sourceId ?? ''}", nameHint="${nameHint ?? ''}")`);
    return null;
  };

  ipcMain.handle(
    'resolve-audio-source',
    async (_event, opts: { sourceId?: string; nameHint?: string }): Promise<native.AudioApp | null> => {
      if (isWayland) {
        return resolveAudioForWayland(opts.nameHint);
      }
      return resolveAudioForX11(opts.sourceId, opts.nameHint);
    },
  );

  ipcMain.handle('get-capture-context', () => lastCaptureContext);

  ipcMain.handle('clipboard-write-text', (_event, text: string) => {
    try {
      clipboard.writeText(String(text ?? ''));
      return true;
    } catch (err) {
      console.error('clipboard-write-text IPC error:', err);
      return false;
    }
  });

  createWindow();
});

app.on('before-quit', () => {
  stopNativeCapture();
});

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') {
    app.quit();
  }
});
