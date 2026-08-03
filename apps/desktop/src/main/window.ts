import { existsSync } from 'node:fs';
import path from 'node:path';
import { app, BrowserWindow, desktopCapturer, Menu, nativeImage, session, shell } from 'electron';
import { getWindow, isWayland, setMainWindow } from './context';

// Flags must be set before app.whenReady(). Build one combined list because
// appendSwitch stores only the *last* value for the same switch name.
const features: string[] = [];

if (isWayland) {
  features.push('WebRTCPipeWireCapturer');
  features.push('WaylandLinuxDrmSyncobj');
}

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
app.commandLine.appendSwitch('disable-frame-rate-limit');
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

export interface WindowDeps {
  stopNativeCapture: () => void;
  setLastCapturedSourceName: (name: string) => void;
}

export function createWindow({ stopNativeCapture, setLastCapturedSourceName }: WindowDeps) {
  const iconPath = resolveIconPath();
  let icon: Electron.NativeImage | undefined;
  if (iconPath) {
    icon = nativeImage.createFromPath(iconPath);
  }

  const win = new BrowserWindow({
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
  setMainWindow(win);

  win.webContents.on('console-message', (event, _level, message) => {
    const msg =
      typeof event === 'object' && event && 'message' in event
        ? (event as { message: string }).message
        : String(message ?? '');
    if (msg) console.log(`[renderer] ${msg}`);
  });

  win.webContents.setWindowOpenHandler(({ url }) => {
    if (url.startsWith('https:') || url.startsWith('http:')) {
      shell.openExternal(url);
    }
    return { action: 'deny' };
  });

  win.webContents.on('will-navigate', (event, url) => {
    const isDev = Boolean(process.env.VITE_DEV_SERVER_URL);
    if (!isDev || (process.env.VITE_DEV_SERVER_URL && !url.startsWith(process.env.VITE_DEV_SERVER_URL))) {
      event.preventDefault();
      if (url.startsWith('https:') || url.startsWith('http:')) {
        shell.openExternal(url);
      }
    }
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
            click: () => getWindow()?.webContents.toggleDevTools(),
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
  win.autoHideMenuBar = true;
  win.setMenuBarVisibility(false);
  win.maximize();

  // Auto-grant media so the renderer can open the virtual capture mic
  // without an interactive portal prompt after screenshare start.
  session.defaultSession.setPermissionRequestHandler((wc, permission, callback) => {
    const url = wc?.getURL() ?? '';
    const isApp = url.startsWith('file://') || url.startsWith('http://localhost:');
    if ((permission === 'media' || permission === 'display-capture' || permission === 'mediaKeySystem') && isApp) {
      callback(true);
      return;
    }
    callback(false);
  });
  session.defaultSession.setPermissionCheckHandler((wc, permission) => {
    const url = wc?.getURL() ?? '';
    const isApp = url.startsWith('file://') || url.startsWith('http://localhost:');
    return (
      (permission === 'media' || (permission as string) === 'display-capture' || permission === 'mediaKeySystem') &&
      isApp
    );
  });

  session.defaultSession.setDisplayMediaRequestHandler((_request, callback) => {
    desktopCapturer
      .getSources({ types: ['window', 'screen'], thumbnailSize: { width: 0, height: 0 }, fetchWindowIcons: false })
      .then((sources) => {
        if (sources.length === 0) {
          console.warn(
            '[setDisplayMediaRequestHandler] desktopCapturer returned 0 sources, using default screen fallback',
          );
          callback({ video: { id: 'screen:0:0', name: 'Entire Screen' } as Electron.DesktopCapturerSource });
          return;
        }
        // Prefer a window source: this app shares windows, not full screens.
        const source = sources.find((s) => s.id.startsWith('window')) ?? sources[0];
        setLastCapturedSourceName(source.name);
        console.log(`[setDisplayMediaRequestHandler] storing source name="${source.name}" (id=${source.id})`);
        callback({ video: source });
      })
      .catch((err) => {
        console.error('[setDisplayMediaRequestHandler] getSources failed, using fallback:', err);
        callback({ video: { id: 'screen:0:0', name: 'Entire Screen' } as Electron.DesktopCapturerSource });
      });
  });

  const devServerUrl = process.env.VITE_DEV_SERVER_URL;
  if (devServerUrl) {
    win.loadURL(devServerUrl);
  } else {
    win.loadFile(path.join(__dirname, '../renderer/index.html'));
  }

  win.on('closed', () => {
    setMainWindow(null);
    stopNativeCapture();
  });

  // Windows fires this on logoff/shutdown instead of before-quit — native
  // sessions must be torn down there too.
  win.on('session-end', () => {
    stopNativeCapture();
  });
}
