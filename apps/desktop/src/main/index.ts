import { createReadStream, existsSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { stat } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import * as nativeLiveKit from '@slopcast/native-livekit';
import * as native from '@slopcast/native-rust';
import { type StreamSettings, sanitizeStreamSettings } from '@slopcast/shared-types';
import { loadConfig } from '@slopcast/shared-types/config';
import {
  app,
  BrowserWindow,
  clipboard,
  desktopCapturer,
  dialog,
  ipcMain,
  Menu,
  nativeImage,
  protocol,
  session,
} from 'electron';

const appConfig = loadConfig();

let mainWindow: BrowserWindow | null = null;
let lastCapturedSourceName: string | null = null;

interface CaptureContext {
  de: 'unknown' | 'kde' | 'gnome';
  mediaName: string | null;
  sourceType: 'monitor' | 'window' | 'unknown';
  videoNodeCount: number;
  screencastNodeId: number | null;
}

let lastCaptureContext: CaptureContext | null = null;

const allowedFilePaths = new Set<string>();

const isWayland =
  process.platform === 'linux' && (process.env.XDG_SESSION_TYPE === 'wayland' || !!process.env.WAYLAND_DISPLAY);

const detectDesktopEnvironment = (): CaptureContext['de'] => {
  const de = (process.env.XDG_CURRENT_DESKTOP ?? '').toUpperCase();
  if (de.includes('KDE')) return 'kde';
  if (de.includes('GNOME')) return 'gnome';
  return 'unknown';
};

// ── Stream Settings Persistence ─────────────────────────────────────────
// Stored as JSON in Electron's per-platform user-data directory
// (%APPDATA%/<app-name> on Windows, ~/.config/<app-name> on Linux,
// ~/Library/Application Support/<app-name> on macOS).
const STREAM_SETTINGS_FILE = 'stream-settings.json';
let streamSettingsCache: StreamSettings | null = null;

const streamSettingsPath = (): string => path.join(app.getPath('userData'), STREAM_SETTINGS_FILE);

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
// DO NOT re-add --no-zygote. In Electron 43 this flag forces posix_spawn() instead of
// fork() for GPU child processes, which prevents PipeWire thread loops from being inherited.
// The null pw_thread_loop* that results triggers a CHECK failure → SIGTRAP on screen-share
// start. See Electron issue #43824 and commit f6a46aa (which first removed the flag).

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
    screencastNodeId: raw.screencastNodeId ?? null,
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
  try {
    nativeLiveKit.stopVideoTrack();
  } catch (err) {
    console.error('Failed to stop native video capture:', err);
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
  session.defaultSession.setPermissionRequestHandler((wc, permission, callback) => {
    const url = wc?.getURL() ?? '';
    const isApp = url.startsWith('file://') || url.startsWith('http://localhost:');
    if ((permission === 'media' || permission === 'mediaKeySystem') && isApp) {
      callback(true);
      return;
    }
    callback(false);
  });
  session.defaultSession.setPermissionCheckHandler((wc, permission) => {
    const url = wc?.getURL() ?? '';
    const isApp = url.startsWith('file://') || url.startsWith('http://localhost:');
    return (permission === 'media' || permission === 'mediaKeySystem') && isApp;
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

  let audioDataCallbackRegistered = false;
  let dmabufCallbackRegistered = false;

  function registerDmabufCallback() {
    if (dmabufCallbackRegistered) return;
    try {
      native.setDmabufCallback((_err: Error | null, arg: number[]) => {
        if (!mainWindow || mainWindow.isDestroyed()) return;
        // arg = [fd, width, height, format, pts_lo, pts_hi]
        nativeLiveKit.captureDmabufFrame(arg[0], arg[1], arg[2], arg[3], arg[4], arg[5]);
      });
      dmabufCallbackRegistered = true;
    } catch (err) {
      console.error('Failed to register dmabuf callback:', err);
    }
  }

  function registerAudioDataCallback() {
    if (audioDataCallbackRegistered) return;
    try {
      native.setAudioDataCallback((err: Error | null, arg: number[]) => {
        if (err || !mainWindow || mainWindow.isDestroyed()) return;
        try {
          nativeLiveKit.feedPcm(arg);
        } catch (_feedErr) {
          // Room may not be connected yet — that's fine
        }
      });
      audioDataCallbackRegistered = true;
    } catch (err) {
      console.error('Failed to register audio data callback:', err);
    }
  }

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
      registerAudioDataCallback();
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
      return native.switchAudioCapture(targetId);
    } catch (err) {
      console.error('switch-audio-capture IPC error:', err);
      return false;
    }
  });

  ipcMain.handle('connect-native-room', (_event, livekitUrl: string, token: string) => {
    try {
      nativeLiveKit.connectLivekitRoom(livekitUrl, token);
      return true;
    } catch (err) {
      console.error('connect-native-room IPC error:', err);
      return false;
    }
  });

  ipcMain.handle('disconnect-native-room', () => {
    try {
      nativeLiveKit.disconnectLivekitRoom();
      return true;
    } catch (err) {
      console.error('disconnect-native-room IPC error:', err);
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
    // Ensure lastCaptureContext has DE info even before Layer 3 runs,
    // so the renderer's fallback works if introspection fails entirely.
    const detectedDe = detectDesktopEnvironment();
    if (!lastCaptureContext || lastCaptureContext.de === 'unknown') {
      lastCaptureContext = {
        de: detectedDe,
        sourceType: 'unknown',
        mediaName: null,
        videoNodeCount: 0,
        screencastNodeId: null,
      };
    }

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
      lastCaptureContext = {
        de: detectDesktopEnvironment(),
        mediaName: null,
        sourceType: 'unknown',
        videoNodeCount: 0,
        screencastNodeId: null,
      };
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

  ipcMain.handle('resolve-audio-app-by-name', async (_event, label: string): Promise<native.AudioApp | null> => {
    try {
      return native.resolveAudioAppByName(label);
    } catch (err) {
      console.error('resolve-audio-app-by-name error:', err);
      return null;
    }
  });

  ipcMain.handle('get-capture-context', () => lastCaptureContext);

  // ── Native Video Capture ─────────────────────────────────────────────
  // Video frames are produced by native-rust's PipeWire pw_stream and
  // delivered to native-livekit via the DMA-BUF callback bridge.
  //
  // WebRTCPipeWireCapturer is REQUIRED on Wayland: it enables Chromium's
  // PipeWire backend so desktopCapturer.getSources() creates screencast
  // nodes the Rust layer introspects for video + audio capture. DO NOT
  // REMOVE — the capture pipeline has no other portal-trigger mechanism.
  // (The flag runs in Chromium's renderer process; native-livekit's
  // libwebrtc runs in the main process — separate address spaces.)

  ipcMain.handle('list-screen-sources', () => {
    try {
      // Source enumeration moved out of native-livekit. On X11, Electron's
      // desktopCapturer provides the source list; on Wayland, the portal
      // picker is shown separately. Returning an empty list signals the
      // renderer to show the portal picker.
      return [];
    } catch (err) {
      console.error('list-screen-sources IPC error:', err);
      return [];
    }
  });

  ipcMain.handle(
    'start-native-capture',
    async (
      _event,
      _sourceIndex: number,
      config: { fps: number; width: number; height: number; videoCodec?: string },
    ) => {
      try {
        // Activate the portal on Wayland to create the screencast
        // PipeWire node. On X11, getSources() returns the native list
        // without a portal prompt.
        const sources = await desktopCapturer.getSources({ types: ['window', 'screen'] });
        if (sources.length === 0) {
          return { ok: false, error: 'No capture sources available' };
        }
        const source = sources.find((s) => s.id.startsWith('window')) ?? sources[0];
        lastCapturedSourceName = source.name;
        console.log(`[native-capture] portal source: "${source.name}"`);

        // Discover the screencast node ID from PipeWire now that the
        // portal has created it. On X11 there is no screencast node.
        let nodeId: number | null = null;
        if (isWayland) {
          try {
            const ctx = native.getCaptureContext();
            nodeId = ctx.screencastNodeId ?? null;
            lastCaptureContext = toCaptureContext(ctx);
            console.log(`[native-capture] nodeId=${nodeId} de=${ctx.de} sourceType=${ctx.sourceType}`);
          } catch (ctxErr) {
            console.error('[native-capture] capture context error:', ctxErr);
          }
        }

        nativeLiveKit.startVideoTrack({
          width: config.width,
          height: config.height,
          fps: config.fps,
          videoCodec: config.videoCodec ?? undefined,
        });

        if (nodeId !== null) {
          registerDmabufCallback();
          native.startVideoCapture(nodeId, config.width, config.height, config.fps);
        }

        return { ok: true, nodeId };
      } catch (err) {
        console.error('start-native-capture IPC error:', err);
        return { ok: false, error: String(err) };
      }
    },
  );

  ipcMain.handle('stop-video-capture', () => {
    try {
      return native.stopVideoCapture();
    } catch (err) {
      console.error('stop-video-capture IPC error:', err);
      return false;
    }
  });

  ipcMain.handle('stop-native-capture', () => {
    try {
      nativeLiveKit.stopVideoTrack();
      native.stopVideoCapture();
      return true;
    } catch (err) {
      console.error('stop-native-capture IPC error:', err);
      return false;
    }
  });

  ipcMain.handle('is-native-capture-active', () => {
    try {
      return nativeLiveKit.isVideoTrackActive();
    } catch (err) {
      console.error('is-native-capture-active IPC error:', err);
      return false;
    }
  });

  ipcMain.handle('get-spectator-count', () => {
    try {
      return nativeLiveKit.getSpectatorCount();
    } catch (err) {
      console.error('get-spectator-count IPC error:', err);
      return 0;
    }
  });

  ipcMain.handle('clipboard-write-text', (_event, text: string) => {
    try {
      clipboard.writeText(String(text ?? ''));
      return true;
    } catch (err) {
      console.error('clipboard-write-text IPC error:', err);
      return false;
    }
  });

  // ── Local media protocol ──────────────────────────────────────────────
  // Chromium's sandbox blocks file:// access from the renderer. A custom
  // protocol lets the renderer play back video files the user selected
  // through the file dialog, with a session-scoped allowlist for security.
  protocol.handle('local-media', async (request) => {
    let filePath: string;
    try {
      const url = new URL(request.url);
      filePath = fileURLToPath(url);
    } catch {
      return new Response('Bad Request', { status: 400 });
    }
    const resolvedPath = realpathSync(filePath);
    if (!allowedFilePaths.has(resolvedPath)) {
      return new Response('Forbidden', { status: 403 });
    }
    const homeDir = app.getPath('home');
    if (!resolvedPath.startsWith(homeDir + path.sep) && resolvedPath !== homeDir) {
      return new Response('Forbidden', { status: 403 });
    }
    try {
      const fileStat = await stat(resolvedPath);
      const ext = path.extname(resolvedPath).toLowerCase();
      const mimeTypes: Record<string, string> = {
        '.mp4': 'video/mp4',
        '.webm': 'video/webm',
        '.ogg': 'video/ogg',
        '.ogv': 'video/ogg',
      };
      const contentType = mimeTypes[ext] ?? 'video/mp4';
      return new Response(createReadStream(resolvedPath) as unknown as ReadableStream, {
        status: 200,
        headers: {
          'Content-Type': contentType,
          'Content-Length': String(fileStat.size),
          'Accept-Ranges': 'bytes',
        },
      });
    } catch {
      return new Response('Not Found', { status: 404 });
    }
  });

  ipcMain.handle('select-video-file', async () => {
    if (!mainWindow) return null;
    const result = await dialog.showOpenDialog(mainWindow, {
      properties: ['openFile'],
      filters: [{ name: 'Video Files', extensions: ['mp4', 'webm', 'ogg', 'ogv'] }],
    });
    if (result.canceled || result.filePaths.length === 0) return null;
    const resolved = realpathSync(result.filePaths[0]);
    allowedFilePaths.clear();
    allowedFilePaths.add(resolved);
    return { filePath: resolved, fileName: path.basename(resolved) };
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
