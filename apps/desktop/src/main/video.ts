import * as nativeLiveKit from '@slopcast/native-livekit';
import * as native from '@slopcast/native-rust';
import { desktopCapturer, ipcMain } from 'electron';
import { stopAudioMeteringPush } from './audio';
import { getWindow, type MainContext } from './context';

interface CaptureContext {
  de: 'unknown' | 'kde' | 'gnome';
  mediaName: string | null;
  sourceType: 'monitor' | 'window' | 'region' | 'unknown';
  videoNodeCount: number;
  screencastNodeId: number | null;
}

let lastCaptureContext: CaptureContext | null = null;
let lastCapturedSourceName: string | null = null;

export const setLastCapturedSourceName = (name: string): void => {
  lastCapturedSourceName = name;
};

const detectDesktopEnvironment = (): CaptureContext['de'] => {
  const de = (process.env.XDG_CURRENT_DESKTOP ?? '').toUpperCase();
  if (de.includes('KDE')) return 'kde';
  if (de.includes('GNOME')) return 'gnome';
  return 'unknown';
};

export function toCaptureContext(raw: native.CaptureContext): CaptureContext {
  return {
    de: raw.de === 'kde' || raw.de === 'gnome' ? raw.de : 'unknown',
    sourceType: raw.sourceType === 'monitor' || raw.sourceType === 'window' ? raw.sourceType : 'unknown',
    mediaName: raw.mediaName ?? null,
    videoNodeCount: raw.videoNodeCount,
    screencastNodeId: raw.screencastNodeId ?? null,
  };
}

let dmabufCallbackRegistered = false;

export function registerDmabufCallback() {
  if (dmabufCallbackRegistered) return;
  try {
    native.setDmabufCallback((_err: Error | null, arg: number[]) => {
      const win = getWindow();
      if (!win || win.isDestroyed()) return;
      // arg = [fd, width, height, format, pts_lo, pts_hi]
      // The fd is owned by PipeWire's buffer pool (webrtc-sys never closes it),
      // so on failure we simply skip the frame; never close it here.
      try {
        nativeLiveKit.captureDmabufFrame(arg[0], arg[1], arg[2], arg[3], arg[4], arg[5]);
      } catch (frameErr) {
        console.error('captureDmabufFrame error:', frameErr);
      }
    });
    dmabufCallbackRegistered = true;
  } catch (err) {
    console.error('Failed to register dmabuf callback:', err);
  }
}

let audioDataCallbackRegistered = false;

export function registerAudioDataCallback() {
  if (audioDataCallbackRegistered) return;
  try {
    native.setAudioDataCallback((err: Error | null, arg: Buffer) => {
      const win = getWindow();
      if (err || !win || win.isDestroyed()) return;
      try {
        win.webContents.send('audio-pcm-data', arg);
      } catch (_sendErr) {
        // Window may be navigating or destroyed
      }
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

export function stopNativeCapture() {
  stopAudioMeteringPush();
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

const resolveAudioForWayland = async (
  ctx: MainContext,
  nameHint: string | undefined,
): Promise<native.AudioApp | null> => {
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
      const app = await ctx.native.resolveAudioAppForCapturedWindow();
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
      const app = await ctx.native.resolveAudioAppByName(hint);
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
    const captureContext = await ctx.native.getCaptureContext();
    lastCaptureContext = toCaptureContext(captureContext);
    console.log(
      `[resolve-audio-source] Wayland context: de=${lastCaptureContext.de} sourceType=${lastCaptureContext.sourceType} mediaName="${lastCaptureContext.mediaName ?? ''}" videoNodes=${lastCaptureContext.videoNodeCount}`,
    );
    if (lastCaptureContext.sourceType === 'monitor' || lastCaptureContext.sourceType === 'region') {
      return null;
    }
    if (captureContext.app) {
      console.log(`[resolve-audio-source] Wayland context-match → "${captureContext.app.name}"`);
      return captureContext.app;
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

const resolveAudioForX11 = async (
  ctx: MainContext,
  sourceId: string | undefined,
  nameHint: string | undefined,
): Promise<native.AudioApp | null> => {
  // Layer 1: _NET_WM_PID via X11 window ID.
  if (sourceId?.startsWith('window:')) {
    const windowId = parseInt(sourceId.split(':')[1], 10);
    if (!Number.isNaN(windowId)) {
      try {
        const app = await ctx.native.resolveAudioAppForX11Window(windowId);
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
      const app = await ctx.native.resolveAudioAppByName(nameHint);
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

export function registerVideoHandlers(ctx: MainContext) {
  ipcMain.handle('get-desktop-sources', async () => {
    const sources = await desktopCapturer.getSources({
      types: ['screen', 'window'],
      thumbnailSize: { width: 300, height: 180 },
      fetchWindowIcons: false,
    });
    return sources.map((s) => ({
      id: s.id,
      name: s.name,
      thumbnail: s.thumbnail.toDataURL(),
    }));
  });

  ipcMain.handle(
    'resolve-audio-source',
    async (_event, opts: { sourceId?: string; nameHint?: string }): Promise<native.AudioApp | null> => {
      if (ctx.isWayland) {
        return resolveAudioForWayland(ctx, opts.nameHint);
      }
      return resolveAudioForX11(ctx, opts.sourceId, opts.nameHint);
    },
  );

  ipcMain.handle('get-capture-context', () => lastCaptureContext);

  // Fresh PipeWire introspection (not the cached lastCaptureContext):
  // xdg-desktop-portal screencast metadata + KWin window resolution + matched
  // audio app for the captured window.
  ipcMain.handle('inspect-capture-context', async () => {
    try {
      return await ctx.native.getCaptureContext();
    } catch (err) {
      console.error('inspect-capture-context IPC error:', err);
      return null;
    }
  });

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
        const sources = await desktopCapturer.getSources({
          types: ['window', 'screen'],
          thumbnailSize: { width: 0, height: 0 },
          fetchWindowIcons: false,
        });
        if (sources.length === 0) {
          return { ok: false, error: 'No capture sources available' };
        }
        const source = sources.find((s) => s.id.startsWith('window')) ?? sources[0];
        lastCapturedSourceName = source.name;
        console.log(`[native-capture] portal source: "${source.name}"`);

        // Discover the screencast node ID from PipeWire now that the
        // portal has created it. On X11 there is no screencast node.
        let nodeId: number | null = null;
        if (ctx.isWayland) {
          try {
            const captureContext = await ctx.native.getCaptureContext();
            nodeId = captureContext.screencastNodeId ?? null;
            lastCaptureContext = toCaptureContext(captureContext);
            console.log(
              `[native-capture] nodeId=${nodeId} de=${captureContext.de} sourceType=${captureContext.sourceType}`,
            );
          } catch (ctxErr) {
            console.error('[native-capture] capture context error:', ctxErr);
          }
        }

        ctx.nativeLiveKit.startVideoTrack({
          width: config.width,
          height: config.height,
          fps: config.fps,
          videoCodec: config.videoCodec ?? undefined,
        });

        if (nodeId !== null) {
          ctx.registerDmabufCallback();
          ctx.native.startVideoCapture(nodeId, config.width, config.height, config.fps);
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
      return ctx.native.stopVideoCapture();
    } catch (err) {
      console.error('stop-video-capture IPC error:', err);
      return false;
    }
  });

  ipcMain.handle('stop-native-capture', () => {
    try {
      ctx.nativeLiveKit.stopVideoTrack();
      ctx.native.stopVideoCapture();
      return true;
    } catch (err) {
      console.error('stop-native-capture IPC error:', err);
      return false;
    }
  });

  ipcMain.handle('is-native-capture-active', () => {
    try {
      return ctx.nativeLiveKit.isVideoTrackActive();
    } catch (err) {
      console.error('is-native-capture-active IPC error:', err);
      return false;
    }
  });
}
