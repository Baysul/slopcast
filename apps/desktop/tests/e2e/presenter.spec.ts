// WebdriverIO presenter-phase spec for the e2e harness (MIGRATION §12.2).
//
// Driven by `apps/server/src/e2e-test.ts` as a subprocess. Runs against the
// Tauri app binary built with the `e2e` cargo feature; the embedded
// WebDriver server (tauri-plugin-wdio-webdriver) + `browser.tauri.execute`
// (tauri-plugin-wdio) power the flow:
//
//   1. Platform assertion (fail-fast Wayland gate only in portal mode)
//   2. Create Live Room → extract the room code from `span.font-mono`
//   3. Start Screenshare → preview canvas appears → Go Live → LIVE
//      (Stop Screenshare visible). The capture route is chosen by the
//      backend: `SLOPCAST_E2E_CAPTURE=synthetic` (default, headless — no
//      portal picker) or `portal` (a human answers the picker).
//   4. Presenter telemetry: get_native_telemetry + get_video_capture_stats
//      sampled twice ~2 s apart; videoFramesSent must advance, the outbound
//      codec must match E2E_CODEC, and previewFramesSent > 0 (proves the
//      JPEG preview emitter ran)
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
  videoCodec?: string | null;
  encoderImplementation?: string | null;
}

interface CaptureStats {
  framesPushed: number;
  previewFramesSent: number;
}

interface PhaseResult {
  ok: boolean;
  /** True only once the presenter is actually live: sets the handoff signal
   * the harness waits on before starting the spectator, so an early Wayland
   * assertion (test 1) alone can never hand off an unstarted session. */
  handoffReady: boolean;
  roomCode: string;
  shareUrl: string;
  isWayland: boolean;
  captureMode: string;
  codec: string;
  gpuReport: GpuInfo | null;
  previewFramesSent: number;
  videoFramesSent: number;
  videoBytesSent: number;
  videoCodecReported: string | null;
  encoderImplementation: string | null;
  captureFramesPushed: number;
  telemetryFlowing: boolean;
  errors: string[];
}

const phaseJsonPath = process.env.E2E_PHASE_JSON;
const releaseFlagPath = process.env.E2E_RELEASE_FLAG;
const websiteUrl = process.env.E2E_WEBSITE_URL;
const captureMode = process.env.E2E_CAPTURE === 'portal' ? 'portal' : 'synthetic';
const codec = process.env.E2E_CODEC ?? 'h264';
if (!phaseJsonPath || !releaseFlagPath || !websiteUrl) {
  throw new Error('E2E_PHASE_JSON, E2E_RELEASE_FLAG and E2E_WEBSITE_URL must be set');
}

const ROOM_CREATE_TIMEOUT_MS = 30_000;
const PREVIEW_TIMEOUT_MS = captureMode === 'portal' ? 60_000 : 20_000;
const TELEMETRY_SAMPLE_GAP_MS = 2000;
const HOLD_TIMEOUT_MS = 180_000;

const phase: PhaseResult = {
  ok: false,
  handoffReady: false,
  roomCode: '',
  shareUrl: '',
  isWayland: false,
  captureMode,
  codec,
  gpuReport: null,
  previewFramesSent: 0,
  videoFramesSent: 0,
  videoBytesSent: 0,
  videoCodecReported: null,
  encoderImplementation: null,
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
  // The WDIO tauri plugin stringifies the script and re-evaluates it inside the
  // webview, so closure variables are lost ("Can't find variable: command").
  // command/args must be forwarded as execute() arguments, not captured.
  return browser.tauri.execute(
    (tauri: TauriApi, cmd: string, invokeArgs: Record<string, unknown> | undefined) =>
      tauri.core.invoke(cmd, invokeArgs) as Promise<T>,
    command,
    args,
  );
}

/// The outbound codec must match the requested one — this pins the whole
/// codec path (picker → config → publish → SFU) per E2E_CODEC pass — and H264
/// must use a hardware encoder (VA-API/NVENC on Linux, Media Foundation on
/// Windows) when one is available; the other codecs are software by design.
function assertCodecTelemetry(phase: PhaseResult): void {
  const expectedMime = `video/${codec.toUpperCase()}`;
  assert(
    (phase.videoCodecReported ?? '').toUpperCase() === expectedMime.toUpperCase(),
    `Outbound codec mismatch: requested ${expectedMime}, reported ${phase.videoCodecReported ?? 'null'}`,
  );
  if (codec !== 'h264') return;
  const impl = phase.encoderImplementation ?? '';
  console.log(`[e2e] h264 encoder implementation: ${impl || '(not yet reported)'}`);
  assert(
    /VAAPI|NVENC|Media\s*Foundation|VideoToolbox/i.test(impl),
    `H264 was not hardware-encoded (encoderImplementation=${impl || 'empty'})`,
  );
}

/// Samples the preview canvas in-page: the canvas uses a WebGL2 context, so
/// its pixels are copied into a fresh 2d canvas (drawImage works across
/// context types) before reading. Self-contained — the WDIO tauri plugin
/// stringifies this function and re-evaluates it inside the webview.
///
/// The synthetic pattern is eight vertical color bars with a white box in
/// rows [h/8, h/4), so the sampler can distinguish:
/// - no content at all (`hasContent`),
/// - a channel swap or grayscale conversion (`barsCorrect`, bar colors are
///   sampled below the box band),
/// - an upside-down render (`upright`, the white box must sit in the top
///   quarter, never the bottom quarter).
async function samplePreviewCanvas(): Promise<{
  hasContent: boolean;
  barsCorrect: boolean;
  upright: boolean;
}> {
  const empty = { hasContent: false, barsCorrect: false, upright: false };
  const canvas = document.querySelector('canvas[aria-label="Live screenshare preview"]');
  if (!canvas) return empty;
  // Wait briefly for the first frame to be drawn by the WebGL path.
  await new Promise((r) => setTimeout(r, 500));
  const copy = document.createElement('canvas');
  copy.width = canvas.width;
  copy.height = canvas.height;
  const ctx = copy.getContext('2d');
  if (!ctx) return empty;
  ctx.drawImage(canvas, 0, 0);
  const { data, width, height } = ctx.getImageData(0, 0, copy.width, copy.height);
  if (width < 640 || height < 360) return empty;

  const pixel = (x: number, y: number): [number, number, number] => {
    const i = (y * width + x) * 4;
    return [data[i], data[i + 1], data[i + 2]];
  };

  let nonBlack = 0;
  let total = 0;
  for (let i = 0; i < data.length; i += 16) {
    total += 1;
    if (Math.max(data[i], data[i + 1], data[i + 2]) > 16) nonBlack += 1;
  }

  // Bar centers at y = 3/4 height, below the moving-box band (rows [h/8, h/4)).
  const y = Math.floor(height * 0.75);
  const bar = (x: number, want: [boolean, boolean, boolean]): boolean => {
    const [r, g, b] = pixel(x, y);
    const ok = (v: number, active: boolean): boolean => (active ? v > 200 : v < 60);
    return ok(r, want[0]) && ok(g, want[1]) && ok(b, want[2]);
  };
  // Synthetic bars at 640-wide preview: red at x=440, blue at x=520, green
  // at x=280, cyan at x=200.
  const barsCorrect =
    bar(440, [true, false, false]) && // red
    bar(520, [false, false, true]) && // blue
    bar(280, [false, true, false]) && // green
    bar(200, [false, true, true]); // cyan

  // Flip check: count white pixels (all channels > 200) in the top-quarter
  // band vs the bottom-quarter band. The white box lives in rows [h/8, h/4)
  // when upright; an upside-down render moves it to rows [3h/4, 7h/8). Bar 0
  // is also white, but it contributes equally to both bands, so the box's
  // constant ~80px decides the comparison regardless of its x position.
  const whiteIn = (y0: number, y1: number): number => {
    let count = 0;
    for (let yy = y0; yy < y1; yy += 2) {
      for (let xx = 0; xx < width; xx += 2) {
        const [r, g, b] = pixel(xx, yy);
        if (r > 200 && g > 200 && b > 200) count += 1;
      }
    }
    return count;
  };
  const whiteTop = whiteIn(Math.floor(height / 8) + 5, Math.floor(height / 4));
  const whiteBottom = whiteIn(Math.floor((3 * height) / 4) + 5, Math.floor((7 * height) / 8));
  const upright = whiteTop > whiteBottom * 1.05;

  return { hasContent: total > 0 && nonBlack / total > 0.1, barsCorrect, upright };
}

const snapshotPresenterTelemetry = async (): Promise<{
  telemetry: NativeTelemetry;
  stats: CaptureStats;
}> => {
  const telemetry = await tauriInvoke<NativeTelemetry>('get_native_telemetry');
  const stats = await tauriInvoke<CaptureStats>('get_video_capture_stats');
  return { telemetry, stats };
};

describe('Slopcast presenter phase (Tauri)', () => {
  // A test that dies with an uncaught error (not an assert) must still land
  // in phase.errors immediately — otherwise the harness can hand off a
  // partial phase (e.g. a room without a live stream) as if it were OK.
  afterEach(function () {
    if (this.currentTest?.state === 'failed' && this.currentTest.err) {
      recordError(this.currentTest.err.message);
    }
  });

  it('runs on a supported session', async () => {
    const info = await tauriInvoke<PlatformInfo>('get_platform_info');
    phase.isWayland = info.isWayland;
    writePhase();
    if (captureMode === 'portal') {
      assert(info.isWayland, 'Wayland required — Slopcast is Wayland-only (D2)');
    } else {
      console.log(`[e2e] synthetic capture mode — Wayland not required (session=${info.platform})`);
    }
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
    // Synthetic mode: the backend feeds test-pattern frames (no portal
    // picker). Portal mode: the runner must answer the picker for frames to
    // flow.
    const startBtn = browser.$('button=Start Screenshare');
    await startBtn.waitForDisplayed({ timeout: ROOM_CREATE_TIMEOUT_MS });
    await startBtn.click();
    if (captureMode === 'portal') {
      console.log('[e2e] portal picker should now be visible — select a source');
    }

    const previewCanvas = browser.$('canvas[aria-label="Live screenshare preview"]');
    await previewCanvas.waitForExist({ timeout: PREVIEW_TIMEOUT_MS });
    console.log(`[e2e] preview canvas visible (mode=${captureMode})`);

    // The preview must actually draw: sample the canvas pixels in-page and
    // require visible content, correct bar colors (no channel swap or
    // grayscale conversion) and an upright image (the white box must be in
    // the top quarter, not the bottom).
    const preview = await browser.tauri.execute(samplePreviewCanvas);
    assert(preview.hasContent, 'Preview canvas has no visible frame content');
    assert(preview.barsCorrect, 'Preview colors are wrong (channel swap or grayscale conversion)');
    assert(preview.upright, 'Preview is rendered upside down');

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

    const t0 = await snapshotPresenterTelemetry();
    await new Promise((r) => setTimeout(r, TELEMETRY_SAMPLE_GAP_MS));
    const t1 = await snapshotPresenterTelemetry();

    phase.videoFramesSent = t1.telemetry.videoFramesSent ?? 0;
    phase.videoBytesSent = t1.telemetry.videoBytesSent ?? 0;
    phase.videoCodecReported = t1.telemetry.videoCodec ?? null;
    phase.encoderImplementation = t1.telemetry.encoderImplementation ?? null;
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
    assertCodecTelemetry(phase);
  });

  it('probes GPU info via probe_gpu_info', async () => {
    const gpu = await tauriInvoke<GpuInfo>('probe_gpu_info');
    phase.gpuReport = gpu;
    // Last validation write before the hold: hand off to the spectator with
    // the complete phase (room + telemetry + GPU), not an early partial.
    phase.handoffReady = true;
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
