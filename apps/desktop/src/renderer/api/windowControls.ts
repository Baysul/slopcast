import type { UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';

const unavailableOps = new Set<string>();

const warnUnavailable = (op: string, cause: unknown): void => {
  if (unavailableOps.has(op)) return;
  unavailableOps.add(op);
  console.warn(`[window] "${op}" unavailable, using fallback:`, cause);
};

const currentWindow = (): ReturnType<typeof getCurrentWindow> | null => {
  try {
    return getCurrentWindow();
  } catch (err) {
    warnUnavailable('getCurrentWindow', err);
    return null;
  }
};

export const windowControls = {
  minimize: async (): Promise<boolean> => {
    const win = currentWindow();
    if (!win) return false;
    try {
      await win.minimize();
      return true;
    } catch (err) {
      warnUnavailable('minimize', err);
      return false;
    }
  },
  toggleMaximize: async (): Promise<boolean> => {
    const win = currentWindow();
    if (!win) return false;
    try {
      await win.toggleMaximize();
      return true;
    } catch (err) {
      warnUnavailable('toggleMaximize', err);
      return false;
    }
  },
  close: async (): Promise<boolean> => {
    const win = currentWindow();
    if (!win) return false;
    try {
      await win.close();
      return true;
    } catch (err) {
      warnUnavailable('close', err);
      return false;
    }
  },
  startDragging: async (): Promise<boolean> => {
    const win = currentWindow();
    if (!win) return false;
    try {
      await win.startDragging();
      return true;
    } catch (err) {
      warnUnavailable('startDragging', err);
      return false;
    }
  },
  isMaximized: async (): Promise<boolean> => {
    const win = currentWindow();
    if (!win) return false;
    try {
      return await win.isMaximized();
    } catch (err) {
      warnUnavailable('isMaximized', err);
      return false;
    }
  },
  onResized: async (callback: () => void): Promise<UnlistenFn> => {
    const win = currentWindow();
    if (!win) return () => undefined;
    try {
      return await win.onResized(callback);
    } catch (err) {
      warnUnavailable('onResized', err);
      return () => undefined;
    }
  },
};
