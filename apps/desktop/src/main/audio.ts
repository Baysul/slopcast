import * as native from '@slopcast/native-rust';
import { WAVE_EPSILON } from '@slopcast/shared-types';
import { ipcMain } from 'electron';
import { getWindow, type MainContext } from './context';

let lastPushedWave = new Map<number, number[]>();

// Sends only when a column actually moved; the native meter reports the live
// waveform, so skipping quiet ticks loses no data. Epsilon shared with the
// renderer store so the two filters can never drift apart.
function waveChanged(prev: number[] | undefined, next: number[]): boolean {
  if (!prev || prev.length !== next.length) return true;
  for (let i = 0; i < next.length; i++) {
    if (Math.abs((prev[i] ?? 0) - (next[i] ?? 0)) > WAVE_EPSILON) {
      return true;
    }
  }
  return false;
}

function pushWaveIfChanged(wc: Electron.WebContents, waves: native.AudioAppWave[]) {
  const changed = waves.some(({ id, columns }) => waveChanged(lastPushedWave.get(id), columns));
  if (!changed) return;
  lastPushedWave = new Map(waves.map(({ id, columns }) => [id, columns]));
  wc.send('audio-wave-update', waves);
}

export function stopAudioMeteringPush() {
  lastPushedWave = new Map();
}

let waveCallbackRegistered = false;

export function registerWaveCallback() {
  if (waveCallbackRegistered) return;
  try {
    native.setAudioWaveCallback((err: Error | null, waves: native.AudioAppWave[]) => {
      const win = getWindow();
      if (err || !win || win.isDestroyed()) return;
      try {
        pushWaveIfChanged(win.webContents, waves);
      } catch (_sendErr) {
        // Window may be navigating or destroyed
      }
    });
    waveCallbackRegistered = true;
  } catch (err) {
    console.error('Failed to register audio wave callback:', err);
  }
}

export function registerAudioHandlers(ctx: MainContext) {
  ipcMain.handle('get-audio-apps', async () => {
    try {
      return await ctx.native.listAudioApplications();
    } catch (err) {
      console.error('get-audio-apps IPC error:', err);
      return [];
    }
  });

  ipcMain.handle('dump-audio-sources', async () => {
    try {
      return await ctx.native.dumpAudioSources();
    } catch (err) {
      console.error('dump-audio-sources IPC error:', err);
      return [];
    }
  });

  ipcMain.handle('start-audio-capture', (_event, targetId: number | string) => {
    try {
      ctx.registerAudioDataCallback();
      return ctx.native.startAudioCapture(targetId);
    } catch (err) {
      console.error('start-audio-capture IPC error:', err);
      return false;
    }
  });

  ipcMain.handle('stop-audio-capture', () => {
    try {
      return ctx.native.stopAudioCapture();
    } catch (err) {
      console.error('stop-audio-capture IPC error:', err);
      return false;
    }
  });

  ipcMain.handle('switch-audio-capture', (_event, targetId: number | string) => {
    try {
      return ctx.native.switchAudioCapture(targetId);
    } catch (err) {
      console.error('switch-audio-capture IPC error:', err);
      return false;
    }
  });

  ipcMain.handle('start-audio-metering', () => {
    try {
      ctx.registerWaveCallback();
      return ctx.native.startAudioMetering();
    } catch (err) {
      console.error('start-audio-metering IPC error:', err);
      return false;
    }
  });

  ipcMain.handle('stop-audio-metering', () => {
    try {
      stopAudioMeteringPush();
      return ctx.native.stopAudioMetering();
    } catch (err) {
      console.error('stop-audio-metering IPC error:', err);
      return false;
    }
  });
}
