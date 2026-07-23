import { execFileSync } from 'node:child_process';
import path from 'node:path';
import * as native from '@screen-share/native-rust';
import { app, BrowserWindow, clipboard, desktopCapturer, ipcMain, Menu, session } from 'electron';

let mainWindow: BrowserWindow | null = null;
let lastCapturedSourceName: string | null = null;

interface CaptureContext {
  de: 'unknown' | 'kde' | 'gnome';
  mediaName: string | null;
  sourceType: 'monitor' | 'window' | 'unknown';
  videoNodeCount: number;
}

let lastCaptureContext: CaptureContext | null = null;

const isLinux = process.platform === 'linux';
// Wayland sessions must capture through xdg-desktop-portal: Chromium's own
// window enumeration does not work there, the portal presents the native
// window/screen picker of the desktop environment instead.
const isWayland = isLinux && (process.env.XDG_SESSION_TYPE === 'wayland' || !!process.env.WAYLAND_DISPLAY);

// Route WebRTC desktop capture through PipeWire/xdg-desktop-portal on
// Wayland. Must be set before the app is ready.
if (isWayland) {
  app.commandLine.appendSwitch('enable-features', 'WebRTCPipeWireCapturer');
}

// ── Hardware-Accelerated Video Encoding ─────────────────────────────────
// These flags must be set before app.whenReady() — they are consumed by
// Chromium's GPU process during early initialisation.
//
// IMPORTANT: Electron/Chromium's appendSwitch stores the *last* value for
// the same switch name, so we must build ONE combined enable-features list.
const features: string[] = [];

// WebRTC PipeWire capturer (Wayland only).  Included in both the early
// switch above (so it's seen immediately) and the combined list below.
if (isWayland) {
  features.push('WebRTCPipeWireCapturer');
}

// ── Platform-specific hardware acceleration features ──────────────────
//
// Chromium 131+ renamed VAAPI feature flags from "VaapiVideo*" to
// "AcceleratedVideo*".  Electron 43 ships Chromium ~150, so we MUST use
// the new names.  See:
//   https://issues.chromium.org/40225939
//   https://dev.to/archerallstars/chrome-flags-latest-2024-update-34k1
//
switch (process.platform) {
  case 'linux':
    // VAAPI-based accelerated video encode (Intel / AMD / nouveau).
    // Feature name: kAcceleratedVideoEncodeLinux → "AcceleratedVideoEncoder"
    // (base::FEATURE_DISABLED_BY_DEFAULT, must be opted in).
    features.push('AcceleratedVideoEncoder');

    // VAAPI decode via GL interop.
    features.push('AcceleratedVideoDecodeLinuxGL');

    // On Wayland, zero-copy image import from VAAPI to GL gives a
    // significant memory/throughput improvement.
    if (isWayland) {
      features.push('AcceleratedVideoDecodeLinuxZeroCopyGL');
    }

    // Bypass the GPU driver allow-list — required for AMD Mesa/RADV
    // and some Intel GEN7+ setups.
    features.push('VaapiIgnoreDriverChecks');

    // OOP rasterisation reduces main-thread load.
    features.push('CanvasOopRasterization');
    break;

  case 'win32':
    // D3D11 Video encode → NVENC / AMD AMF / Intel QSV depending on GPU.
    // These features are enabled by default on Chromium 130+ but we list
    // them explicitly for defence in depth.
    features.push('D3D11VideoEncoder', 'D3D11VideoDecoder');
    break;

  // macOS: VideoToolbox HW encode is enabled by default in Chromium 130+
  // and needs no extra feature flag.
}

// ── Cross-platform quality and rendering features ─────────────────────
// Enable GPU rasterization on all platforms for smoother compositing.
app.commandLine.appendSwitch('enable-gpu-rasterization');

// Bypass the GPU blocklist — some systems (e.g. VMs, older GPUs) have it
// enabled by default, which disables all GPU acceleration.
app.commandLine.appendSwitch('ignore-gpu-blocklist');

// Commit the combined feature list.  This overrides the earlier
// WebRTCPipeWireCapturer-only call above.
app.commandLine.appendSwitch('enable-features', features.join(','));

// Low-latency GPU video decoding pipeline (VP9 / AV1).
app.commandLine.appendSwitch('enable-low-latency-video-decoder');

// Never throttle timers when the window is in the background — essential for
// stable screenshare encoding when the presenter navigates to another window.
app.commandLine.appendSwitch('disable-background-timer-throttling');

// Prevent Chromium from lowering the renderer process priority when the
// window is hidden/minimized.
app.commandLine.appendSwitch('disable-renderer-backgrounding');

function createWindow() {
  mainWindow = new BrowserWindow({
    width: 1100,
    height: 900,
    title: 'ScreenShare Desktop Presenter',
    backgroundColor: '#090d16',
    webPreferences: {
      nodeIntegration: false,
      contextIsolation: true,
      preload: path.join(__dirname, 'preload.js'),
      backgroundThrottling: false,
    },
  });

  // Minimal application menu whose accelerators (including Ctrl+Shift+I for
  // DevTools) are active even though the menu bar is auto-hidden.
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
}

function stopNativeCapture() {
  try {
    native.stopAudioCapture();
    console.log('🛑 Audio capture stopped');
  } catch (err) {
    console.error('Failed to stop audio capture:', err);
  }
}

app.whenReady().then(() => {
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

    const audioApps = native.getAudioApplications();
    console.log(`🔊 Detected ${audioApps.length} active audio applications:`);
    audioApps.forEach((app: native.AudioApp) => {
      console.log(`  - [ID: ${app.id}] ${app.name} (Process ID: ${app.processId})`);
    });
  } catch (err) {
    console.error('❌ Native audio engine error:', err);
  }

  // Display media requests (`navigator.mediaDevices.getDisplayMedia`) are
  // answered here. On Wayland the actual window/screen selection happens in
  // the xdg-desktop-portal dialog of the desktop environment — the source
  // handed to Chromium only selects the portal-backed capturer, the portal
  // itself decides which window the user picks.
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
  ipcMain.handle('get-platform-info', () => ({
    platform: process.platform,
    isWayland,
  }));

  ipcMain.handle('get-audio-apps', () => {
    try {
      return native.getAudioApplications();
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

  ipcMain.handle('get-desktop-sources', async () => {
    const sources = await desktopCapturer.getSources({ types: ['screen', 'window'] });
    return sources.map((s) => ({
      id: s.id,
      name: s.name,
      thumbnail: s.thumbnail.toDataURL(),
    }));
  });

  /**
   * Auto-detects the audio source for a given window selection.
   *
   * Resolution strategies by platform (each falls through to the next):
   *
   * **Wayland:**
   *   1. PipeWire introspection — scan the registry for the portal's
   *      Video/Source node and extract the captured window's identity
   *      from its `portal.screencast.*` / `window.name` properties.
   *   2. Name hint — fuzzy-match `opts.nameHint` (the track label from
   *      `getDisplayMedia`) against active audio app names via Rust.
   *
   * **X11:**
   *   1. `_NET_WM_PID` — map the X11 window ID (from `opts.sourceId`)
   *      to a PID and find the matching PipeWire audio application.
   *   2. Name hint — fuzzy-match `opts.nameHint` (the source name from
   *      `desktopCapturer`) against active audio app names via Rust.
   */
  ipcMain.handle(
    'resolve-audio-source',
    async (_event, opts: { sourceId?: string; nameHint?: string }): Promise<native.AudioApp | null> => {
      let app: native.AudioApp | null = null;

      if (isWayland) {
        // ---- Layer 1: PipeWire introspection ----
        try {
          app = native.resolveAudioAppForCapturedWindow();
          if (app) {
            console.log(`[resolve-audio-source] Wayland PW-introspect → "${app.name}" (PID ${app.processId})`);
            return app;
          }
        } catch (err) {
          console.error('resolve-audio-source Wayland introspection error:', err);
        }

        // ---- Layer 2: Name matching via Rust ----
        const nameHint = opts.nameHint || lastCapturedSourceName;
        if (nameHint) {
          try {
            app = native.resolveAudioAppByName(nameHint);
            if (app) {
              console.log(`[resolve-audio-source] Wayland name-match "${nameHint}" → "${app.name}"`);
              return app;
            }
          } catch (err) {
            console.error('resolve-audio-source Wayland name-match error:', err);
          }
        }

        // ---- Layer 3: pw-dump subprocess (get full node properties) ----
        try {
          const output = execFileSync('pw-dump', [], {
            timeout: 2000,
            stdio: ['ignore', 'pipe', 'pipe'],
          });
          const state = JSON.parse(output.toString());
          let matchApp: native.AudioApp | null = null;
          const ctx: CaptureContext = {
            de: 'unknown',
            mediaName: null,
            sourceType: 'unknown',
            videoNodeCount: 0,
          };

          for (const obj of state) {
            const info = obj.info;
            const props: Record<string, string> = info?.props ?? {};
            const mc: string = props['media.class'] ?? '';
            if (mc.startsWith('Stream/Output/Video')) {
              ctx.videoNodeCount++;
              const mn: string = props['media.name'] ?? '';
              ctx.mediaName = mn;
              console.log(`[resolve-audio-source] pw-dump vid node id=${obj.id}: media.name="${mn}"`);

              if (mn.startsWith('kwin-screencast-')) {
                ctx.de = 'kde';
                const suffix = mn.slice('kwin-screencast-'.length);
                // Monitor identifiers look like "DP-3", "HDMI-1", "eDP-1", etc.
                ctx.sourceType = /^[A-Z]+-\d+$/.test(suffix) ? 'monitor' : 'window';

                // On KDE window capture, try matching the suffix (window hex id)
                // as a last resort — it likely won't match, but no harm trying.
                if (ctx.sourceType === 'window' && suffix) {
                  try {
                    matchApp = native.resolveAudioAppByName(suffix);
                  } catch (_) {}
                }
              } else if (props['portal.screencast.application']) {
                ctx.de = 'gnome';
              }
            }
          }

          lastCaptureContext = ctx;
          console.log(
            `[resolve-audio-source] Wayland pw-dump: de=${ctx.de} sourceType=${ctx.sourceType} mediaName="${ctx.mediaName}" videoNodes=${ctx.videoNodeCount}`,
          );

          if (matchApp) {
            console.log(`[resolve-audio-source] pw-dump name-match → "${matchApp.name}"`);
            return matchApp;
          }
        } catch (err) {
          console.error('[resolve-audio-source] pw-dump fallback error:', err);
          lastCaptureContext = {
            de: 'unknown',
            mediaName: null,
            sourceType: 'unknown',
            videoNodeCount: 0,
          };
        }

        console.log(
          `[resolve-audio-source] Wayland: no match (introspect=null, nameHint="${opts.nameHint ?? ''}", lastSource="${lastCapturedSourceName ?? ''}")`,
        );
        return null;
      }

      // ---- X11 Layer 1: _NET_WM_PID ----
      if (opts.sourceId?.startsWith('window:')) {
        const windowIdStr = opts.sourceId.split(':')[1];
        const windowId = parseInt(windowIdStr, 10);
        if (!Number.isNaN(windowId)) {
          try {
            app = native.resolveAudioAppForX11Window(windowId);
            if (app) {
              console.log(`[resolve-audio-source] X11 PID-match: window ${windowId} → "${app.name}"`);
              return app;
            }
          } catch (err) {
            console.error('resolve-audio-source X11 error:', err);
          }
        }
      }

      // ---- X11 Layer 2: Name matching via Rust ----
      if (opts.nameHint) {
        try {
          app = native.resolveAudioAppByName(opts.nameHint);
          if (app) {
            console.log(`[resolve-audio-source] X11 name-match "${opts.nameHint}" → "${app.name}"`);
            return app;
          }
        } catch (err) {
          console.error('resolve-audio-source X11 name-match error:', err);
        }
      }

      console.log(
        `[resolve-audio-source] X11: no match (sourceId="${opts.sourceId ?? ''}", nameHint="${opts.nameHint ?? ''}")`,
      );
      return null;
    },
  );

  ipcMain.handle('get-capture-context', () => lastCaptureContext);

  // navigator.clipboard is unreliable in Electron without a secure context +
  // user gesture; write through the main-process clipboard instead.
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
  // Destroy the PipeWire virtual sink and its links before the process exits.
  stopNativeCapture();
});

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') {
    app.quit();
  }
});
