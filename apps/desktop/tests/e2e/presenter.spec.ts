// WebdriverIO presenter-phase spec for the e2e harness (MIGRATION §12.2).
//
// Driven by `apps/server/src/e2e-test.ts` as a subprocess. Runs against the
// Tauri app binary built with the `e2e` cargo feature; the embedded
// WebDriver server (tauri-plugin-wdio-webdriver) + `browser.tauri.execute`
// (tauri-plugin-wdio) power the flow:
//
//   1. Wayland assertion (fail fast)
//   2. Create Live Room → extract the room code from `span.font-mono`
//   3. Start Screenshare → portal picker (answered by the runner) → preview
//      canvas appears → Go Live → LIVE (Stop Screenshare visible)
//   4. Presenter telemetry: get_native_telemetry + get_video_capture_stats
//      sampled twice ~2 s apart; videoFramesSent must advance and
//      previewFramesSent > 0 (proves the §9.1 preview emitter ran)
//   5. GPU diagnostics via probe_gpu_info (D5) — softwareRasterizer must be
//      false and eglVendor present
//   6. Hold: keep the app alive until the harness releases it after the
//      spectator phase — the tauri-service kills the app at session end, so
//      this spec must still be running while Playwright verifies the stream
//
// Progress is written to E2E_PHASE_JSON after every step; the harness polls
// that file and writes E2E_RELEASE_FLAG to end the hold early (immediately
// when the phase already failed). Global `browser` comes from the WDIO
// runtime; Node 24 runs this spec natively (erasable-only TS syntax).

import { existsSync, writeFileSync } from 'node:fs';

interface TauriApi {
  core: { invoke: (command: string, args?: unknown) => Promise<unknown> };
}

interface ElementApi {
  waitForExist(opts?: { timeout?: number }): Promise<void>;
  waitForDisplayed(opts?: { timeout?: number }): Promise<void>;
  click(): Promise<void>;
  getText(): Promise<string>;
  isExisting(): Promise<boolean>;
}

declare const browser: {
  $: (selector: string) => ElementApi;
  tauri: {
    execute: <T>(script: (tauri: TauriApi) => Promise<T> | T) => Promise<T>;
  };
};

interface PlatformInfo {
  platform: string;
  isWayland: boolean;
}

interface GpuInfo {
  eglVendor: string | null;
  glRenderer: string | null;
  glVersion: string | null;
  softwareRasterizer: boolean;
}

interface NativeTelemetry {
  videoFramesSent?: number;
  videoBytesSent?: number;
}

interface CaptureStats {
  framesPushed: number;
  previewFramesSent: number;
}

interface PhaseResult {
  ok: boolean;
  roomCode: string;
  shareUrl: string;
  isWayland: boolean;
  gpuReport: GpuInfo | null;
  previewFramesSent: number;
  videoFramesSent: number;
  videoBytesSent: number;
  captureFramesPushed: number;
  telemetryFlowing: boolean;
  errors: string[];
}

const phaseJsonPath = process.env.E2E_PHASE_JSON;
const releaseFlagPath = process.env.E2E_RELEASE_FLAG;
const websiteUrl = process.env.E2E_WEBSITE_URL;
if (!phaseJsonPath || !releaseFlagPath || !websiteUrl) {
  throw new Error('E2E_PHASE_JSON, E2E_RELEASE_FLAG and E2E_WEBSITE_URL must be set');
}

const ROOM_CREATE_TIMEOUT_MS = 30_000;
const PICKER_TIMEOUT_MS = 60_000;
const TELEMETRY_SAMPLE_GAP_MS = 2000;
const HOLD_TIMEOUT_MS = 180_000;

const phase: PhaseResult = {
  ok: false,
  roomCode: '',
  shareUrl: '',
  isWayland: false,
  gpuReport: null,
  previewFramesSent: 0,
  videoFramesSent: 0,
  videoBytesSent: 0,
  captureFramesPushed: 0,
  telemetryFlowing: false,
  errors: [],
};

function writePhase(): void {
  phase.ok = phase.errors.length === 0;
  writeFileSync(phaseJsonPath, JSON.stringify(phase, null, 2));
}

function recordError(message: string): void {
  phase.errors.push(message);
  console.error(`[e2e] ${message}`);
  writePhase();
}

function assert(condition: boolean, message: string): void {
  if (!condition) {
    recordError(message);
    throw new Error(message);
  }
}

function tauriInvoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  return browser.tauri.execute((tauri: TauriApi) => tauri.core.invoke(command, args) as Promise<T>);
}

describe('Slopcast presenter phase (Tauri)', () => {
  it('runs on a Wayland session', async () => {
    const info = await tauriInvoke<PlatformInfo>('get_platform_info');
    phase.isWayland = info.isWayland;
    writePhase();
    assert(info.isWayland, 'Wayland required — Slopcast is Wayland-only (D2)');
  });

  it('creates a live room and extracts the room code', async () => {
    const createBtn = browser.$('button=Create Live Room');
    await createBtn.waitForDisplayed({ timeout: ROOM_CREATE_TIMEOUT_MS });
    await createBtn.click();

    const codeSpan = browser.$('span.font-mono');
    await codeSpan.waitForExist({ timeout: ROOM_CREATE_TIMEOUT_MS });
    const roomCode = (await codeSpan.getText()).trim();
    phase.roomCode = roomCode;
    phase.shareUrl = `${websiteUrl}/room/${roomCode}`;
    writePhase();

    assert(roomCode.length > 0, `Failed to extract a room code from the UI (got "${roomCode}")`);
    console.log(`[e2e] room created: code=${roomCode} url=${phase.shareUrl}`);
  });

  it('starts the pre-roll capture, shows the preview and goes live', async () => {
    // Answer the xdg-desktop-portal picker when it appears — the runner must
    // select a source for frames (and thus the preview canvas) to flow.
    const startBtn = browser.$('button=Start Screenshare');
    await startBtn.waitForDisplayed({ timeout: ROOM_CREATE_TIMEOUT_MS });
    await startBtn.click();
    console.log('[e2e] portal picker should now be visible — select a source');

    const previewCanvas = browser.$('canvas[aria-label="Live screenshare preview"]');
    await previewCanvas.waitForExist({ timeout: PICKER_TIMEOUT_MS });
    console.log('[e2e] preview canvas visible');

    const goLiveBtn = browser.$('button=Go Live');
    await goLiveBtn.waitForDisplayed({ timeout: 10_000 });
    await goLiveBtn.click();

    const stopBtn = browser.$('button=Stop Screenshare');
    await stopBtn.waitForExist({ timeout: 30_000 });
    console.log('[e2e] live — Stop Screenshare visible');
  });

  it('samples presenter telemetry and proves the preview emitter ran', async function () {
    const stopBtn = browser.$('button=Stop Screenshare');
    if (!(await stopBtn.isExisting())) {
      this.skip();
      return;
    }

    const snapshot = async (): Promise<{ telemetry: NativeTelemetry; stats: CaptureStats }> => {
      const telemetry = await tauriInvoke<NativeTelemetry>('get_native_telemetry');
      const stats = await tauriInvoke<CaptureStats>('get_video_capture_stats');
      return { telemetry, stats };
    };

    const t0 = await snapshot();
    await new Promise((r) => setTimeout(r, TELEMETRY_SAMPLE_GAP_MS));
    const t1 = await snapshot();

    phase.videoFramesSent = t1.telemetry.videoFramesSent ?? 0;
    phase.videoBytesSent = t1.telemetry.videoBytesSent ?? 0;
    phase.captureFramesPushed = t1.stats.framesPushed;
    phase.previewFramesSent = t1.stats.previewFramesSent;
    phase.telemetryFlowing =
      (t1.telemetry.videoFramesSent ?? 0) > (t0.telemetry.videoFramesSent ?? 0) &&
      (t1.telemetry.videoBytesSent ?? 0) > 0 &&
      t1.stats.framesPushed > 0;
    writePhase();

    console.log(
      `[e2e] telemetry: framesSent ${t0.telemetry.videoFramesSent ?? 'null'} -> ` +
        `${t1.telemetry.videoFramesSent ?? 'null'}, bytesSent=${phase.videoBytesSent}, ` +
        `captureFramesPushed=${phase.captureFramesPushed}, previewFramesSent=${phase.previewFramesSent}`,
    );

    assert(phase.telemetryFlowing, 'Presenter video telemetry did not advance (frames/bytes stalled)');
    assert(phase.previewFramesSent > 0, 'No preview frames were emitted (previewFramesSent stayed at 0)');
  });

  it('probes GPU info via probe_gpu_info', async () => {
    const gpu = await tauriInvoke<GpuInfo>('probe_gpu_info');
    phase.gpuReport = gpu;
    writePhase();

    assert(gpu != null, 'GPU probe returned no data');
    assert(gpu.eglVendor != null && gpu.eglVendor.length > 0, 'GPU probe reported no EGL vendor');
    assert(gpu.softwareRasterizer === false, 'GPU is software-rendered (llvmpipe/softpipe)');
    console.log(`[e2e] gpu: vendor=${gpu.eglVendor} renderer=${gpu.glRenderer} version=${gpu.glVersion}`);
  });

  it('keeps the app alive until the spectator phase releases it', async () => {
    const deadline = Date.now() + HOLD_TIMEOUT_MS;
    while (Date.now() < deadline) {
      if (existsSync(releaseFlagPath)) {
        console.log('[e2e] release flag seen — ending presenter session');
        return;
      }
      await new Promise((r) => setTimeout(r, 2000));
    }
    recordError('Presenter hold timed out before the spectator phase released it');
  });

  after(() => {
    writePhase();
  });
});
