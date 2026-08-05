// Preview transport + end-to-end preview benchmark (Phase 2).
//
// Measures, in the real app:
//   1. Transport: Tauri's raw channel throughput + arrival jitter for the
//      two candidate preview payloads — option 1's JPEG frames (~100 KB)
//      vs option 2's raw RGBA frames (921 KB at 640×360).
//   2. End-to-end: synthetic capture → libjpeg-turbo encode → raw channel →
//      webview decode → canvas draw, reporting effective fps and p50/p95/p99
//      latency (native pts → arrival → drawn).
//
// Output: `test-output/bench-preview.json` (cwd is apps/desktop under wdio,
// so the repo-root test-output dir is two levels up).
//
// Run (after `pnpm --filter desktop tauri build --features e2e`):
//
// ```sh
// SLOPCAST_E2E_CAPTURE=synthetic \
//   pnpm --filter desktop exec wdio run ./wdio.conf.ts \
//   --spec tests/e2e/bench-preview.spec.ts
// ```

import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname } from 'node:path';

interface TauriApi {
  core: { invoke: (command: string, args?: unknown) => Promise<unknown> };
}

interface ElementApi {
  waitForDisplayed(opts?: { timeout?: number }): Promise<void>;
  waitForExist(opts?: { timeout?: number }): Promise<void>;
  click(): Promise<void>;
  getText(): Promise<string>;
}

declare const browser: {
  $: (selector: string) => ElementApi;
  tauri: {
    execute: <T>(script: (tauri: TauriApi, ...args: unknown[]) => Promise<T> | T, ...args: unknown[]) => Promise<T>;
  };
};

interface BenchResult {
  transport: Record<string, TransportRun>;
  previewEndToEnd: {
    framesDrawn: number;
    fps: number;
    arrivalToDrawMs: { p50: number; p95: number; p99: number };
    nativePreviewFramesSent: number;
  };
  errors: string[];
}

interface TransportRun {
  payloadBytes: number;
  framesRequested: number;
  framesDelivered: number;
  effectiveFps: number;
  mbPerSec: number;
  interArrivalMs: { p50: number; p95: number; p99: number; max: number };
}

const BENCH_OUT = '../../test-output/bench-preview.json';
const PUSH_TIMEOUT_MS = 45_000;

const result: BenchResult = {
  transport: {},
  previewEndToEnd: {
    framesDrawn: 0,
    fps: 0,
    arrivalToDrawMs: { p50: 0, p95: 0, p99: 0 },
    nativePreviewFramesSent: 0,
  },
  errors: [],
};

function recordError(message: string): void {
  result.errors.push(message);
  console.error(`[bench] ${message}`);
}

function assert(condition: boolean, message: string): void {
  if (!condition) {
    recordError(message);
    throw new Error(message);
  }
}

function percentile(sorted: number[], p: number): number {
  if (sorted.length === 0) return 0;
  const index = Math.min(sorted.length - 1, Math.ceil((p / 100) * sorted.length) - 1);
  return sorted[Math.max(0, index)];
}

function writeResult(): void {
  mkdirSync(dirname(BENCH_OUT), { recursive: true });
  writeFileSync(BENCH_OUT, JSON.stringify(result, null, 2));
  console.log(`[bench] wrote ${BENCH_OUT}`);
}

function tauriInvoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  return browser.tauri.execute(
    (tauri: TauriApi, cmd: string, invokeArgs: Record<string, unknown> | undefined) =>
      tauri.core.invoke(cmd, invokeArgs) as Promise<T>,
    command,
    args,
  );
}

/// Registers a channel in-page (the wdio proxy only exposes `invoke`, so the
/// real `Channel` class comes from `window.__TAURI__` — `withGlobalTauri` is
/// enabled) and pushes `count` payloads of `size` bytes through it, recording
/// arrival timestamps and payload sizes. Returns the arrival stats. Note:
/// everything the script needs must be inlined — closure variables are lost
/// when the wdio plugin stringifies and re-evaluates it in the webview.
async function transportRun(payloadBytes: number, frames: number, intervalMs: number): Promise<TransportRun> {
  return browser.tauri.execute(
    async (tauri: TauriApi, size: number, count: number, cadenceMs: number, timeoutMs: number) => {
      type Arrival = { t: number; len: number };
      const arrivals: Arrival[] = [];
      const percentile = (sorted: number[], p: number): number => {
        if (sorted.length === 0) return 0;
        const index = Math.min(sorted.length - 1, Math.ceil((p / 100) * sorted.length) - 1);
        return sorted[Math.max(0, index)];
      };
      const Channel = (
        window.__TAURI__ as unknown as {
          core: { Channel: new (cb: (raw: { message?: ArrayBuffer }) => void) => unknown };
        }
      ).core.Channel;
      const channel = new Channel((raw: ArrayBuffer) => {
        arrivals.push({ t: performance.now(), len: raw?.byteLength ?? 0 });
      });
      await tauri.core.invoke('bench_register_channel', { channel });
      await tauri.core.invoke('bench_push_frames', { count, size, intervalMs: cadenceMs });
      const deadline = Date.now() + timeoutMs;
      while (arrivals.length < count && Date.now() < deadline) {
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
      const times = arrivals.map((a) => a.t).sort((a, b) => a - b);
      const gaps = times
        .slice(1)
        .map((t, i) => t - times[i])
        .sort((a, b) => a - b);
      const delivered = arrivals.length;
      const elapsedMs = times.length > 1 ? times[times.length - 1] - times[0] : 0;
      return {
        framesRequested: count,
        framesDelivered: delivered,
        payloadBytes: size,
        effectiveFps: elapsedMs > 0 ? (delivered / elapsedMs) * 1000 : 0,
        mbPerSec: elapsedMs > 0 ? (delivered * size) / elapsedMs / 1000 : 0,
        interArrivalMs: {
          p50: percentile(gaps, 50),
          p95: percentile(gaps, 95),
          p99: percentile(gaps, 99),
          max: gaps.length > 0 ? gaps[gaps.length - 1] : 0,
        },
      } as TransportRun;
    },
    payloadBytes,
    frames,
    intervalMs,
    PUSH_TIMEOUT_MS,
  );
}

describe('Preview transport and end-to-end benchmark', () => {
  it('measures raw channel throughput for the two payload sizes', async function () {
    this.timeout(180_000);
    // 640×360 RGBA (option 2) and a representative q=90 JPEG (option 1),
    // both at ~60 fps cadence for 3 s.
    const jpeg = await transportRun(100_000, 180, 16);
    result.transport.jpeg = jpeg;
    const rgba = await transportRun(640 * 360 * 4, 180, 16);
    result.transport.rgba = rgba;
    assert(jpeg.framesDelivered > 0, `no JPEG-size frames delivered (${jpeg.framesDelivered}/${jpeg.framesRequested})`);
    assert(rgba.framesDelivered > 0, `no RGBA-size frames delivered (${rgba.framesDelivered}/${rgba.framesRequested})`);
    console.log(`[bench] transport jpeg: ${JSON.stringify(jpeg)}`);
    console.log(`[bench] transport rgba: ${JSON.stringify(rgba)}`);
    writeResult();
  });

  it('measures the end-to-end preview path (capture → JPEG → channel → canvas)', async function () {
    this.timeout(180_000);
    // Arm the renderer's bench hook (main.tsx records arrival, PreviewCanvas
    // records draw completion), then drive the real preview pipeline with
    // the synthetic capture source.
    await browser.tauri.execute(() => {
      (
        window as unknown as {
          __PREVIEW_BENCH__: boolean;
          __PREVIEW_BENCH_DATA__: Array<[number, number, number | null]>;
        }
      ).__PREVIEW_BENCH__ = true;
      (window as unknown as { __PREVIEW_BENCH_DATA__: Array<[number, number, number | null]> }).__PREVIEW_BENCH_DATA__ =
        [];
    });
    // Drive the real UI flow: the preview canvas only mounts once the app's
    // captureStage leaves 'idle', which requires a room + the Start button.
    const createBtn = browser.$('button=Create Live Room');
    await createBtn.waitForDisplayed({ timeout: 30_000 });
    await createBtn.click();
    const codeSpan = browser.$('span.font-mono');
    await codeSpan.waitForExist({ timeout: 30_000 });
    const roomCode = (await codeSpan.getText()).trim();
    assert(roomCode.length > 0, 'no room code extracted');
    const startBtn = browser.$('button=Start Screenshare');
    await startBtn.waitForDisplayed({ timeout: 30_000 });
    await startBtn.click();
    for (let i = 0; i < 5; i++) {
      await new Promise((resolve) => setTimeout(resolve, 2000));
      const mid = await tauriInvoke<{ previewFramesSent: number; framesDequeued: number }>('get_video_capture_stats');
      console.log(`[bench] t+${(i + 1) * 2}s stats: ${JSON.stringify(mid)}`);
    }
    const previewCanvas = browser.$('canvas[aria-label="Live screenshare preview"]');
    const canvasPresent = await previewCanvas.isExisting().catch(() => false);
    console.log(`[bench] preview canvas present after 10s: ${canvasPresent}`);
    if (!canvasPresent) {
      await previewCanvas.waitForExist({ timeout: 20_000 });
    }

    const sampleMs = 6000;
    await new Promise((resolve) => setTimeout(resolve, sampleMs));
    await tauriInvoke<boolean>('stop_native_capture');

    const data = await browser.tauri.execute<Array<[number, number, number | null]>>(() => {
      const w = window as unknown as { __PREVIEW_BENCH_DATA__?: Array<[number, number, number | null]> };
      return w.__PREVIEW_BENCH_DATA__ ?? [];
    });
    const stats = await tauriInvoke<{ previewFramesSent: number }>('get_video_capture_stats');

    const drawn = data.filter((entry) => entry[2] !== null);
    // Note: end-to-end native-emit → arrival latency is not reported because
    // the native pts clock and performance.now() have different zero points.
    const arrivalToDraw = drawn
      .map(([, arrivalMs, drawMs]) => (drawMs as number) - arrivalMs)
      .filter((v) => v >= 0)
      .sort((a, b) => a - b);
    const first = drawn.length > 0 ? (drawn[0][1] as number) : 0;
    const last = drawn.length > 0 ? (drawn[drawn.length - 1][1] as number) : 0;

    result.previewEndToEnd = {
      framesDrawn: drawn.length,
      fps: last > first && drawn.length > 1 ? drawn.length / ((last - first) / 1000) : 0,
      arrivalToDrawMs: {
        p50: percentile(arrivalToDraw, 50),
        p95: percentile(arrivalToDraw, 95),
        p99: percentile(arrivalToDraw, 99),
      },
      nativePreviewFramesSent: stats.previewFramesSent,
    };
    assert(drawn.length > 0, 'no preview frames reached the canvas');
    console.log(`[bench] preview end-to-end: ${JSON.stringify(result.previewEndToEnd)}`);
    writeResult();
  });
});
