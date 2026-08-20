// Preview transport + end-to-end preview benchmark (Phase 2), Playwright-CDP
// port of the old WDIO bench spec.
//
// Measures, in the real app:
//   1. Transport: Tauri's raw channel throughput + arrival jitter for the
//      two candidate preview payloads — option 1's JPEG frames (~100 KB)
//      vs option 2's raw RGBA frames (921 KB at 640×360).
//   2. End-to-end: synthetic capture → libjpeg-turbo encode → raw channel →
//      webview decode → canvas draw, reporting effective fps and p50/p95/p99
//      latency (native pts → arrival → drawn).
//
// Output: `test-output/bench-preview.json` (cwd is apps/desktop, so the
// repo-root test-output dir is two levels up).
//
// Run (after `VITE_E2E=1 pnpm --filter desktop tauri build --features e2e`):
//
// ```sh
// SLOPCAST_E2E_CAPTURE=synthetic XDG_CONFIG_HOME=$(pwd)/test-output/e2e-userdata \
//   ../../target/release/slopcast &
// node tests/e2e/bench-preview.playwright.ts
// ```

import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname } from 'node:path';
import { type Browser, chromium, type Page } from 'playwright';

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
const CDP_URL = process.env.E2E_CDP_URL ?? 'http://127.0.0.1:9222';

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

function tauriInvoke<T>(page: Page, command: string, args?: Record<string, unknown>): Promise<T> {
  return page.evaluate(
    ({ cmd, invokeArgs }) => {
      const tauriWindow = window as unknown as {
        __TAURI__?: { core?: { invoke?: (c: string, a?: Record<string, unknown>) => Promise<unknown> } };
      };
      if (!tauriWindow.__TAURI__?.core?.invoke) throw new Error('window.__TAURI__.core.invoke unavailable');
      return tauriWindow.__TAURI__.core.invoke(cmd, invokeArgs);
    },
    { cmd: command, invokeArgs: args },
  ) as Promise<T>;
}

/// Pushes `count` payloads of `size` bytes through a raw Tauri channel
/// (registered in-page via `window.__TAURI__.core.Channel` — withGlobalTauri
/// is on) and records arrival timestamps and payload sizes.
async function transportRun(
  page: Page,
  payloadBytes: number,
  frames: number,
  intervalMs: number,
): Promise<TransportRun> {
  return page.evaluate(
    async ({ size, count, cadenceMs, timeoutMs }) => {
      type Arrival = { t: number; len: number };
      const arrivals: Arrival[] = [];
      const percentile = (sorted: number[], p: number): number => {
        if (sorted.length === 0) return 0;
        const index = Math.min(sorted.length - 1, Math.ceil((p / 100) * sorted.length) - 1);
        return sorted[Math.max(0, index)];
      };
      const tauriWindow = window as unknown as {
        __TAURI__: {
          core: {
            Channel: new (cb: (raw: ArrayBuffer) => void) => unknown;
            invoke: (command: string, args?: Record<string, unknown>) => Promise<unknown>;
          };
        };
      };
      const Channel = tauriWindow.__TAURI__.core.Channel;
      const channel = new Channel((raw: ArrayBuffer) => {
        arrivals.push({ t: performance.now(), len: raw?.byteLength ?? 0 });
      });
      await tauriWindow.__TAURI__.core.invoke('bench_register_channel', { channel });
      await tauriWindow.__TAURI__.core.invoke('bench_push_frames', { count, size, intervalMs: cadenceMs });
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
    { size: payloadBytes, count: frames, cadenceMs: intervalMs, timeoutMs: PUSH_TIMEOUT_MS },
  );
}

async function main(): Promise<void> {
  const browser: Browser = await chromium.connectOverCDP(CDP_URL);
  const context = browser.contexts()[0];
  const page = context.pages().find((candidate) => /tauri\.localhost/.test(candidate.url())) ?? context.pages()[0];
  if (!page) throw new Error('CEF exposed no page target over CDP');

  try {
    // 640×360 BGRA (921 KB/frame — the raw preview payload size) and a
    // 100 KB/frame baseline, both at ~60 fps cadence for 3 s.
    const jpeg = await transportRun(page, 100_000, 180, 16);
    result.transport.jpeg = jpeg;
    const rgba = await transportRun(page, 640 * 360 * 4, 180, 16);
    result.transport.rgba = rgba;
    assert(jpeg.framesDelivered > 0, `no JPEG-size frames delivered (${jpeg.framesDelivered}/${jpeg.framesRequested})`);
    assert(rgba.framesDelivered > 0, `no RGBA-size frames delivered (${rgba.framesDelivered}/${rgba.framesRequested})`);
    console.log(`[bench] transport jpeg: ${JSON.stringify(jpeg)}`);
    console.log(`[bench] transport rgba: ${JSON.stringify(rgba)}`);
    writeResult();

    // Arm the renderer's bench hook (main.tsx records arrival, PreviewCanvas
    // records draw completion), then drive the real preview pipeline with
    // the synthetic capture source.
    await page.evaluate(() => {
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
    await page.getByRole('button', { name: 'Create Live Room', exact: true }).click();
    await page.locator('span.font-mono').waitFor({ state: 'attached', timeout: 30_000 });
    await page.getByRole('button', { name: 'Start Screenshare', exact: true }).click();
    for (let i = 0; i < 5; i++) {
      await new Promise((resolve) => setTimeout(resolve, 2000));
      const mid = await tauriInvoke<{ previewFramesSent: number; framesDequeued: number }>(
        page,
        'get_video_capture_stats',
      );
      console.log(`[bench] t+${(i + 1) * 2}s stats: ${JSON.stringify(mid)}`);
    }
    await page.locator('canvas[aria-label="Live screenshare preview"]').waitFor({ state: 'attached', timeout: 20_000 });

    const sampleMs = 6000;
    await new Promise((resolve) => setTimeout(resolve, sampleMs));
    await tauriInvoke<boolean>(page, 'stop_native_capture');

    const data = await page.evaluate(() => {
      const w = window as unknown as { __PREVIEW_BENCH_DATA__?: Array<[number, number, number | null]> };
      return w.__PREVIEW_BENCH_DATA__ ?? [];
    });
    const stats = await tauriInvoke<{ previewFramesSent: number }>(page, 'get_video_capture_stats');

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
  } finally {
    writeResult();
    await browser.close().catch(() => undefined);
  }
}

main().catch((error) => {
  const message = error instanceof Error ? error.message : String(error);
  recordError(message);
  console.error(`[bench] fatal: ${message}`);
  process.exitCode = 1;
});
