import { app, BrowserWindow, ipcMain, desktopCapturer, session, clipboard } from 'electron';
import path from 'path';
import * as native from '@screen-share/native-rust';

let mainWindow: BrowserWindow | null = null;

const isLinux = process.platform === 'linux';
// Wayland sessions must capture through xdg-desktop-portal: Chromium's own
// window enumeration does not work there, the portal presents the native
// window/screen picker of the desktop environment instead.
const isWayland =
  isLinux && (process.env.XDG_SESSION_TYPE === 'wayland' || !!process.env.WAYLAND_DISPLAY);

// Route WebRTC desktop capture through PipeWire/xdg-desktop-portal on
// Wayland. Must be set before the app is ready.
if (isWayland) {
  app.commandLine.appendSwitch('enable-features', 'WebRTCPipeWireCapturer');
}

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
    },
  });

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
   * Two resolution strategies:
   * - `sourceId` (X11):  Extract the X11 window ID from `"window:12345"`,
   *   then use Rust's `resolveAudioAppForX11Window` to map it to a PID via
   *   `_NET_WM_PID` and find the matching PipeWire audio application.
   * - `trackLabel` (Wayland):  Pass the `MediaStreamTrack.label` from
   *   `getDisplayMedia` to Rust's `resolveAudioAppByName` for fuzzy matching
   *   against active audio application names.
   */
  ipcMain.handle(
    'resolve-audio-source',
    async (
      _event,
      opts: { sourceId?: string; trackLabel?: string }
    ): Promise<native.AudioApp | null> => {
      // --- X11: window ID → PID → audio app ---
      if (opts.sourceId && opts.sourceId.startsWith('window:')) {
        const windowIdStr = opts.sourceId.split(':')[1];
        const windowId = parseInt(windowIdStr, 10);
        if (isNaN(windowId)) {
          console.warn('resolve-audio-source: invalid window ID:', opts.sourceId);
          return null;
        }
        try {
          const app = native.resolveAudioAppForX11Window(windowId);
          if (app) {
            console.log(
              `[resolve-audio-source] X11 match: window ${windowId} → PID ${app.processId} → "${app.name}"`
            );
            return app;
          }
          console.log(
            `[resolve-audio-source] X11 window ${windowId}: no audio app found for PID`
          );
        } catch (err) {
          console.error('resolve-audio-source X11 error:', err);
        }
        return null;
      }

      // --- Wayland: track label → name matching → audio app ---
      if (opts.trackLabel) {
        try {
          const app = native.resolveAudioAppByName(opts.trackLabel);
          if (app) {
            console.log(
              `[resolve-audio-source] Wayland match: label "${opts.trackLabel}" → "${app.name}"`
            );
            return app;
          }
          console.log(
            `[resolve-audio-source] Wayland: no match for label "${opts.trackLabel}"`
          );
        } catch (err) {
          console.error('resolve-audio-source Wayland error:', err);
        }
        return null;
      }

      console.warn('resolve-audio-source: no sourceId or trackLabel provided');
      return null;
    }
  );

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
