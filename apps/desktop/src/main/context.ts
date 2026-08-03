import type { BrowserWindow } from 'electron';

let mainWindow: BrowserWindow | null = null;

export const setMainWindow = (win: BrowserWindow | null): void => {
  mainWindow = win;
};

export const getWindow = (): BrowserWindow | null => mainWindow;

export const isWayland =
  process.platform === 'linux' && (process.env.XDG_SESSION_TYPE === 'wayland' || !!process.env.WAYLAND_DISPLAY);

export interface MainContext {
  getWindow: () => BrowserWindow | null;
  native: typeof import('@slopcast/native-rust');
  nativeLiveKit: typeof import('@slopcast/native-livekit');
  isWayland: boolean;
  registerDmabufCallback: () => void;
  registerAudioDataCallback: () => void;
  registerWaveCallback: () => void;
}
