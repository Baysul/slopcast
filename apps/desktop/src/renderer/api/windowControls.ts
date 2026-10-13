import type { UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWindow, PhysicalPosition, PhysicalSize } from '@tauri-apps/api/window';
import type React from 'react';
import { desktopApi } from '@/api/desktop';

export type ResizeDirection =
  | 'East'
  | 'North'
  | 'NorthEast'
  | 'NorthWest'
  | 'South'
  | 'SouthEast'
  | 'SouthWest'
  | 'West';

const MIN_WINDOW_SIZE = 420;

const unavailableOps = new Set<string>();

const warnUnavailable = (op: string, cause: unknown): void => {
  if (unavailableOps.has(op)) return;
  unavailableOps.add(op);
  console.warn(`[window] "${op}" unavailable, using fallback:`, cause);
};

const applyWindowChange = (op: string, change: Promise<unknown>): void => {
  void change.catch((err) => warnUnavailable(op, err));
};

const currentWindow = (): ReturnType<typeof getCurrentWindow> | null => {
  try {
    return getCurrentWindow();
  } catch (err) {
    warnUnavailable('getCurrentWindow', err);
    return null;
  }
};

// Chromium grabs the X pointer for the duration of every press inside the webview, so the window
// manager can never take over an interactive move or resize that starts in the page. Those gestures
// are driven from the renderer on Linux; other platforms keep the window manager paths.
let dragsInRenderer = navigator.userAgent.includes('Linux');
void desktopApi
  .getPlatformInfo()
  .then((info) => {
    dragsInRenderer = info.platform === 'linux';
  })
  .catch(() => undefined);

interface DragStart {
  target: Element;
  pointerId: number;
  screenX: number;
  screenY: number;
}

interface DragContext {
  delta: { x: number; y: number };
  origin: { x: number; y: number; width: number; height: number };
  scale: number;
  win: ReturnType<typeof getCurrentWindow>;
}

const pointerStart = (event: React.PointerEvent<Element>): DragStart => ({
  target: event.currentTarget,
  pointerId: event.pointerId,
  screenX: event.screenX,
  screenY: event.screenY,
});

const beginPointerDrag = async (start: DragStart, update: (context: DragContext) => void): Promise<void> => {
  const win = currentWindow();
  if (!win) return;

  const [position, size, scale] = await Promise.all([win.outerPosition(), win.outerSize(), win.scaleFactor()]);
  const origin = { x: position.x, y: position.y, width: size.width, height: size.height };
  let pending: { x: number; y: number } | null = null;
  let frame = 0;

  const applyPending = (): void => {
    frame = 0;
    if (!pending) return;
    const delta = pending;
    pending = null;
    update({ delta, origin, scale, win });
  };

  const handleMove = (event: PointerEvent): void => {
    pending = { x: event.screenX - start.screenX, y: event.screenY - start.screenY };
    frame ||= requestAnimationFrame(applyPending);
  };

  const finish = (): void => {
    if (frame) cancelAnimationFrame(frame);
    frame = 0;
    pending = null;
    window.removeEventListener('pointermove', handleMove);
    window.removeEventListener('pointerup', finish);
    window.removeEventListener('pointercancel', finish);
    if (start.target.hasPointerCapture(start.pointerId)) {
      start.target.releasePointerCapture(start.pointerId);
    }
  };

  try {
    start.target.setPointerCapture(start.pointerId);
  } catch (err) {
    warnUnavailable('pointer capture', err);
  }
  window.addEventListener('pointermove', handleMove);
  window.addEventListener('pointerup', finish);
  window.addEventListener('pointercancel', finish);
};

const moveWindowFromPointer = (start: DragStart): void => {
  if (!dragsInRenderer) {
    void nativeDragWindow();
    return;
  }

  void beginPointerDrag(start, ({ delta, origin, win }) => {
    applyWindowChange('setPosition', win.setPosition(new PhysicalPosition(origin.x + delta.x, origin.y + delta.y)));
  });
};

const RESIZE_EDGES = {
  East: { horizontal: 1, vertical: 0 },
  North: { horizontal: 0, vertical: -1 },
  NorthEast: { horizontal: 1, vertical: -1 },
  NorthWest: { horizontal: -1, vertical: -1 },
  South: { horizontal: 0, vertical: 1 },
  SouthEast: { horizontal: 1, vertical: 1 },
  SouthWest: { horizontal: -1, vertical: 1 },
  West: { horizontal: -1, vertical: 0 },
} satisfies Record<ResizeDirection, { horizontal: -1 | 0 | 1; vertical: -1 | 0 | 1 }>;

const resizeWindowFromPointer = (direction: ResizeDirection, start: DragStart): void => {
  if (!dragsInRenderer) {
    void nativeResizeWindow(direction);
    return;
  }

  const { horizontal, vertical } = RESIZE_EDGES[direction];
  void beginPointerDrag(start, ({ delta, origin, scale, win }) => {
    const width = Math.max(MIN_WINDOW_SIZE * scale, origin.width + delta.x * horizontal);
    const height = Math.max(MIN_WINDOW_SIZE * scale, origin.height + delta.y * vertical);
    applyWindowChange('setSize', win.setSize(new PhysicalSize(width, height)));
    if (horizontal < 0 || vertical < 0) {
      const x = horizontal < 0 ? origin.x + origin.width - width : origin.x;
      const y = vertical < 0 ? origin.y + origin.height - height : origin.y;
      applyWindowChange('setPosition', win.setPosition(new PhysicalPosition(x, y)));
    }
  });
};

const nativeDragWindow = async (): Promise<boolean> => {
  const win = currentWindow();
  if (!win) return false;
  try {
    await win.startDragging();
    return true;
  } catch (err) {
    warnUnavailable('startDragging', err);
    return false;
  }
};

const nativeResizeWindow = async (direction: ResizeDirection): Promise<boolean> => {
  const win = currentWindow();
  if (!win) return false;
  try {
    await win.startResizeDragging(direction);
    return true;
  } catch (err) {
    warnUnavailable('startResize', err);
    return false;
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
  dragFromPointer: (event: React.PointerEvent<Element>): void => moveWindowFromPointer(pointerStart(event)),
  resizeFromPointer: (direction: ResizeDirection, event: React.PointerEvent<Element>): void =>
    resizeWindowFromPointer(direction, pointerStart(event)),
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
