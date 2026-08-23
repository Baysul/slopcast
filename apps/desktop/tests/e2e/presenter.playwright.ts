import { existsSync, writeFileSync } from 'node:fs';
import { type Browser, chromium, type Page } from 'playwright';

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
  videoFramesEncoded?: number;
  videoBytesSent?: number;
  videoCodec?: string | null;
  encoderImplementation?: string | null;
  timestampMs?: number;
}

interface CaptureStats {
  framesPushed: number;
  previewFramesSent: number;
}

interface PhaseResult {
  ok: boolean;
  handoffReady: boolean;
  roomCode: string;
  shareUrl: string;
  isWayland: boolean;
  captureMode: string;
  codec: string;
  gpuReport: GpuInfo | null;
  previewFramesSent: number;
  videoFramesEncoded: number;
  videoBytesSent: number;
  videoCodecReported: string | null;
  encoderImplementation: string | null;
  captureFramesPushed: number;
  telemetryFlowing: boolean;
  telemetryFps: number;
  senderBitrateBps: number;
  senderBitrateSampleMs: number;
  postSubscriptionTelemetryReady: boolean;
  errors: string[];
}

const phaseJsonPath = process.env.E2E_PHASE_JSON;
const releaseFlagPath = process.env.E2E_RELEASE_FLAG;
const stopFlagPath = process.env.E2E_STOP_FLAG;
const stoppedFlagPath = process.env.E2E_STOPPED_FLAG;
const spectatorReadyFlagPath = process.env.E2E_SPECTATOR_READY_FLAG;
const websiteUrl = process.env.E2E_WEBSITE_URL;
const captureMode = process.env.E2E_CAPTURE === 'portal' ? 'portal' : 'synthetic';
const codec = process.env.E2E_CODEC ?? 'h264';
const expectedFps = Number(process.env.E2E_EXPECTED_FPS ?? 60);
const expectedBitrate = Number(process.env.E2E_EXPECTED_BITRATE ?? 20_000_000);
const cdpUrl = process.env.E2E_CDP_URL ?? 'http://127.0.0.1:9222';
if (!phaseJsonPath || !releaseFlagPath || !websiteUrl) {
  throw new Error('E2E_PHASE_JSON, E2E_RELEASE_FLAG and E2E_WEBSITE_URL must be set');
}

const ROOM_CREATE_TIMEOUT_MS = 30_000;
const PREVIEW_TIMEOUT_MS = captureMode === 'portal' ? 60_000 : 20_000;
const TELEMETRY_SAMPLE_GAP_MS = 2000;
const HOLD_TIMEOUT_MS = 180_000;
const CDP_UP_TIMEOUT_MS = 60_000;

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
  videoFramesEncoded: 0,
  videoBytesSent: 0,
  videoCodecReported: null,
  encoderImplementation: null,
  captureFramesPushed: 0,
  telemetryFlowing: false,
  telemetryFps: 0,
  senderBitrateBps: 0,
  senderBitrateSampleMs: 0,
  postSubscriptionTelemetryReady: false,
  errors: [],
};

function writePhase(): void {
  phase.ok = phase.errors.length === 0;
  if (phaseJsonPath) writeFileSync(phaseJsonPath, JSON.stringify(phase, null, 2));
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

async function connectToApp(): Promise<{ browser: Browser; page: Page }> {
  const deadline = Date.now() + CDP_UP_TIMEOUT_MS;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`${cdpUrl}/json/version`);
      if (response.ok) break;
    } catch (error) {
      console.debug('[e2e] CDP endpoint not ready:', error);
    }
    await new Promise((resolve) => setTimeout(resolve, 1000));
  }
  const browser = await chromium.connectOverCDP(cdpUrl);
  const context = browser.contexts()[0];
  const page =
    context.pages().find((candidate) => /tauri\.localhost|localhost:5173/.test(candidate.url())) ?? context.pages()[0];
  if (!page) throw new Error('CEF exposed no page target over CDP');
  return { browser, page };
}

function locatorFor(page: Page, selector: string) {
  const buttonText = /^button=(.+)$/.exec(selector);
  if (buttonText) return page.getByRole('button', { name: buttonText[1], exact: true });
  return page.locator(selector);
}

async function waitForDisplayed(page: Page, selector: string, timeout: number): Promise<void> {
  await locatorFor(page, selector).waitFor({ state: 'visible', timeout });
}

async function waitForExist(page: Page, selector: string, timeout: number): Promise<void> {
  await locatorFor(page, selector).waitFor({ state: 'attached', timeout });
}

async function isExisting(page: Page, selector: string): Promise<boolean> {
  return (await locatorFor(page, selector).count()) > 0;
}

async function getText(page: Page, selector: string): Promise<string> {
  return (await locatorFor(page, selector).innerText()).trim();
}

async function clickElement(page: Page, selector: string): Promise<void> {
  await locatorFor(page, selector).click();
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

function assertCodecTelemetry(phaseResult: PhaseResult): void {
  const expectedMime = `video/${codec.toUpperCase()}`;
  assert(
    (phaseResult.videoCodecReported ?? '').toUpperCase() === expectedMime.toUpperCase(),
    `Outbound codec mismatch: requested ${expectedMime}, reported ${phaseResult.videoCodecReported ?? 'null'}`,
  );
  if (codec !== 'h264' && codec !== 'h265') return;
  const impl = phaseResult.encoderImplementation ?? '';
  console.log(`[e2e] ${codec} encoder implementation: ${impl || '(not yet reported)'}`);
  assert(
    /VAAPI|vah264enc|vah265enc|x265enc|NVENC|Media\s*Foundation|VideoToolbox/i.test(impl),
    `${codec} was not hardware-encoded (encoderImplementation=${impl || 'empty'})`,
  );
}

async function samplePreviewCanvas(
  page: Page,
): Promise<{ hasContent: boolean; barsCorrect: boolean; upright: boolean; horizontal: boolean; barChannels: string }> {
  return page.evaluate(async () => {
    const empty = {
      hasContent: false,
      barsCorrect: false,
      upright: false,
      horizontal: false,
      barChannels: '',
    };
    const canvas = document.querySelector('canvas[aria-label="Live screenshare preview"]') as HTMLCanvasElement | null;
    if (!canvas) return empty;
    await new Promise((resolve) => setTimeout(resolve, 500));
    const copy = document.createElement('canvas');
    copy.width = canvas.width;
    copy.height = canvas.height;
    const ctx = copy.getContext('2d');
    if (!ctx) return empty;
    ctx.drawImage(canvas, 0, 0);
    const { data, width, height } = ctx.getImageData(0, 0, copy.width, copy.height);
    const dumpBars = (): string => {
      const y = Math.floor(height * 0.75);
      const barWidth = width / 8;
      const bars: string[] = [];
      for (let index = 0; index < 8; index++) {
        const x = Math.floor(barWidth * (index + 0.5));
        const i = (y * width + x) * 4;
        bars.push(`[${data[i]},${data[i + 1]},${data[i + 2]}]`);
      }
      return bars.join(' ');
    };
    if (width < 160 || height < 90) return empty;

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

    const y = Math.floor(height * 0.75);
    const barWidth = width / 8;
    const bar = (index: number, want: [boolean, boolean, boolean]): boolean => {
      const x = Math.floor(barWidth * (index + 0.5));
      const [r, g, b] = pixel(x, y);
      const ok = (v: number, active: boolean): boolean => (active ? v > 200 : v < 60);
      return ok(r, want[0]) && ok(g, want[1]) && ok(b, want[2]);
    };
    const barsCorrect =
      bar(5, [true, false, false]) &&
      bar(6, [false, false, true]) &&
      bar(3, [false, true, false]) &&
      bar(2, [false, true, true]);

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

    const horizontal = bar(0, [true, true, true]) && bar(7, [false, false, false]);

    return {
      hasContent: total > 0 && nonBlack / total > 0.1,
      barsCorrect,
      upright,
      horizontal,
      barChannels: dumpBars(),
    };
  });
}

async function snapshotPresenterTelemetry(page: Page): Promise<{
  telemetry: NativeTelemetry;
  stats: CaptureStats;
}> {
  const telemetry = await tauriInvoke<NativeTelemetry>(page, 'get_native_telemetry');
  const stats = await tauriInvoke<CaptureStats>(page, 'get_video_capture_stats');
  return { telemetry, stats };
}

async function readTelemetryBarFps(page: Page): Promise<string> {
  let uiFps = '—';
  for (let i = 0; i < 12; i++) {
    const selector = '[data-testid="telemetry-fps"]';
    if (await isExisting(page, selector)) {
      uiFps = await getText(page, selector);
      const parsed = Number.parseFloat(uiFps);
      if (Number.isFinite(parsed) && parsed > 0) break;
    }
    await new Promise((resolve) => setTimeout(resolve, 1000));
  }
  return uiFps;
}

async function stopShareForSpectatorCheck(page: Page): Promise<void> {
  await waitForDisplayed(page, 'button=Stop Screenshare', 30_000);
  await clickElement(page, 'button=Stop Screenshare');
  await waitForDisplayed(page, 'button=Stop', 10_000);
  await clickElement(page, 'button=Stop');
  await waitForDisplayed(page, 'button=Start Screenshare', 30_000);
  console.log('[e2e] stop completed — presenter back to the idle stage');
}

async function verifyPostSubscriptionTelemetry(page: Page): Promise<void> {
  const start = await snapshotPresenterTelemetry(page);
  await new Promise((resolve) => setTimeout(resolve, 5000));
  const end = await snapshotPresenterTelemetry(page);
  const startBytes = start.telemetry.videoBytesSent ?? 0;
  const endBytes = end.telemetry.videoBytesSent ?? 0;
  const startTimestamp = start.telemetry.timestampMs ?? 0;
  const endTimestamp = end.telemetry.timestampMs ?? 0;
  const bytesDelta = endBytes - startBytes;
  const sampleMs = endTimestamp - startTimestamp;

  phase.videoBytesSent = endBytes;
  phase.senderBitrateSampleMs = sampleMs;
  if (bytesDelta > 0 && sampleMs > 0) {
    phase.senderBitrateBps = (bytesDelta * 8 * 1000) / sampleMs;
  }
  assert(phase.videoBytesSent > 0, 'Presenter RTP bytes stayed at zero after the spectator subscribed');
  assert(
    phase.senderBitrateBps > 0,
    `Presenter RTP bitrate could not be measured (bytesDelta=${bytesDelta}, sampleMs=${sampleMs})`,
  );
  const maximumMeasuredBitrate = expectedBitrate * 1.25;
  assert(
    phase.senderBitrateBps <= maximumMeasuredBitrate,
    `Presenter RTP bitrate ${(phase.senderBitrateBps / 1_000_000).toFixed(2)} Mbps exceeded the configured ` +
      `${(expectedBitrate / 1_000_000).toFixed(2)} Mbps limit plus 25% VBR tolerance`,
  );

  let uiBitrate = '—';
  for (let attempt = 0; attempt < 12; attempt++) {
    const selector = '[data-testid="telemetry-bitrate"]';
    if (await isExisting(page, selector)) {
      uiBitrate = await getText(page, selector);
      const parsed = Number.parseFloat(uiBitrate);
      if (Number.isFinite(parsed) && parsed > 0) break;
    }
    await new Promise((resolve) => setTimeout(resolve, 1000));
  }
  assert(
    Number.parseFloat(uiBitrate) > 0,
    `Telemetry bar never showed live video bitrate after subscription (stuck at "${uiBitrate}")`,
  );
  phase.postSubscriptionTelemetryReady = true;
  writePhase();
  console.log(
    `[e2e] post-subscription telemetry: bytesSent=${phase.videoBytesSent}, ` +
      `measuredBitrate=${(phase.senderBitrateBps / 1_000_000).toFixed(2)}Mbps, uiBitrate="${uiBitrate}"`,
  );
}

async function runStep(name: string, step: () => Promise<void>): Promise<void> {
  console.log(`[e2e] step: ${name}`);
  try {
    await step();
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    if (!phase.errors.some((existing) => existing === message)) {
      recordError(message);
    }
  }
}

async function main(): Promise<void> {
  const { browser, page } = await connectToApp();

  try {
    await runStep('runs on a supported session', async () => {
      const info = await tauriInvoke<PlatformInfo>(page, 'get_platform_info');
      phase.isWayland = info.isWayland;
      writePhase();
      if (captureMode === 'portal') {
        assert(info.isWayland, 'Wayland required — Slopcast is Wayland-only (D2)');
      } else {
        console.log(`[e2e] synthetic capture mode — Wayland not required (session=${info.platform})`);
      }
    });

    await runStep('creates a live room and extracts the room code', async () => {
      await waitForDisplayed(page, 'button=Create Live Room', ROOM_CREATE_TIMEOUT_MS);
      await clickElement(page, 'button=Create Live Room');

      await waitForExist(page, 'span.font-mono', ROOM_CREATE_TIMEOUT_MS);
      const roomCode = await getText(page, 'span.font-mono');
      phase.roomCode = roomCode;
      phase.shareUrl = `${websiteUrl}/room/${roomCode}`;
      writePhase();

      assert(roomCode.length > 0, `Failed to extract a room code from the UI (got "${roomCode}")`);
      console.log(`[e2e] room created: code=${roomCode} url=${phase.shareUrl}`);
    });

    await runStep('starts the pre-roll capture, shows the preview and goes live', async () => {
      await waitForDisplayed(page, 'button=Start Screenshare', ROOM_CREATE_TIMEOUT_MS);
      await clickElement(page, 'button=Start Screenshare');
      if (captureMode === 'portal') {
        console.log('[e2e] portal picker should now be visible — select a source');
      }

      await waitForExist(page, 'canvas[aria-label="Live screenshare preview"]', PREVIEW_TIMEOUT_MS);
      console.log(`[e2e] preview canvas visible (mode=${captureMode})`);

      const preview = await samplePreviewCanvas(page);
      assert(preview.hasContent, 'Preview canvas has no visible frame content');
      assert(
        preview.barsCorrect,
        `Preview colors are wrong (channel swap or grayscale conversion) — bars: ${preview.barChannels}`,
      );
      assert(preview.upright, 'Preview is rendered upside down');
      assert(preview.horizontal, 'Preview is mirrored horizontally');

      await waitForDisplayed(page, 'button=Go Live', 10_000);
      await clickElement(page, 'button=Go Live');

      await waitForExist(page, 'button=Stop Screenshare', 30_000);
      console.log('[e2e] live — Stop Screenshare visible');
    });

    await runStep('samples presenter telemetry and proves the preview emitter ran', async () => {
      if (!(await isExisting(page, 'button=Stop Screenshare'))) {
        return;
      }

      const t0 = await snapshotPresenterTelemetry(page);
      await new Promise((resolve) => setTimeout(resolve, TELEMETRY_SAMPLE_GAP_MS));
      const t1 = await snapshotPresenterTelemetry(page);

      phase.videoFramesEncoded = t1.telemetry.videoFramesEncoded ?? 0;
      phase.videoBytesSent = t1.telemetry.videoBytesSent ?? 0;
      phase.videoCodecReported = t1.telemetry.videoCodec ?? null;
      phase.encoderImplementation = t1.telemetry.encoderImplementation ?? null;
      phase.captureFramesPushed = t1.stats.framesPushed;
      phase.previewFramesSent = t1.stats.previewFramesSent;
      const framesDelta = (t1.telemetry.videoFramesEncoded ?? 0) - (t0.telemetry.videoFramesEncoded ?? 0);
      phase.telemetryFps = Math.round(framesDelta / (TELEMETRY_SAMPLE_GAP_MS / 1000));
      phase.telemetryFlowing = framesDelta > 0 && t1.stats.framesPushed > 0;
      writePhase();

      console.log(
        `[e2e] telemetry: framesEncoded ${t0.telemetry.videoFramesEncoded ?? 'null'} -> ` +
          `${t1.telemetry.videoFramesEncoded ?? 'null'} (${phase.telemetryFps} fps), ` +
          `bytesSent=${phase.videoBytesSent}, ` +
          `captureFramesPushed=${phase.captureFramesPushed}, previewFramesSent=${phase.previewFramesSent}`,
      );

      assert(phase.telemetryFlowing, 'Presenter video telemetry did not advance (frames/capture stalled)');
      const minimumFps = Math.floor(expectedFps * 0.8);
      assert(
        phase.telemetryFps >= minimumFps,
        `Presenter stream ran at ${phase.telemetryFps} fps, expected at least ${minimumFps} fps for a configured ${expectedFps} fps stream`,
      );
      assert(phase.previewFramesSent > 0, 'No preview frames were emitted (previewFramesSent stayed at 0)');

      const uiFps = await readTelemetryBarFps(page);
      console.log(
        `[e2e] telemetry bar shows: Frame Rate="${uiFps}" Capture="${await getText(page, '[data-testid="telemetry-capture"]')}"`,
      );
      const uiFpsValue = Number.parseFloat(uiFps);
      assert(
        Number.isFinite(uiFpsValue) && uiFpsValue > 0,
        `Telemetry bar never showed a live frame rate (stuck at "${uiFps}") — the renderer is not displaying the encoder's framesEncoded`,
      );
      assertCodecTelemetry(phase);
    });

    await runStep('probes GPU info via probe_gpu_info', async () => {
      const gpu = await tauriInvoke<GpuInfo>(page, 'probe_gpu_info');
      phase.gpuReport = gpu;
      phase.handoffReady = true;
      writePhase();

      assert(gpu != null, 'GPU probe returned no data');
      assert(gpu.eglVendor != null && gpu.eglVendor.length > 0, 'GPU probe reported no EGL vendor');
      assert(gpu.softwareRasterizer === false, 'GPU is software-rendered (llvmpipe/softpipe)');
      console.log(`[e2e] gpu: vendor=${gpu.eglVendor} renderer=${gpu.glRenderer} version=${gpu.glVersion}`);
    });

    await runStep('keeps the app alive until the spectator phase releases it', async () => {
      const deadline = Date.now() + HOLD_TIMEOUT_MS;
      let spectatorTelemetryVerified = false;
      while (Date.now() < deadline) {
        if (releaseFlagPath && existsSync(releaseFlagPath)) {
          console.log('[e2e] release flag seen — ending presenter session');
          return;
        }
        if (stopFlagPath && stoppedFlagPath && !existsSync(stoppedFlagPath) && existsSync(stopFlagPath)) {
          await stopShareForSpectatorCheck(page);
          writeFileSync(stoppedFlagPath, 'stopped');
          console.log('[e2e] share stopped — waiting for the spectator check to complete');
        }
        if (spectatorReadyFlagPath && !spectatorTelemetryVerified && existsSync(spectatorReadyFlagPath)) {
          await verifyPostSubscriptionTelemetry(page);
          spectatorTelemetryVerified = true;
        }
        await new Promise((resolve) => setTimeout(resolve, 2000));
      }
      recordError('Presenter hold timed out before the spectator phase released it');
    });
  } finally {
    writePhase();
    await browser.close().catch(() => undefined);
  }
}

main().catch((error) => {
  const message = error instanceof Error ? error.message : String(error);
  recordError(message);
  console.error(`[e2e] fatal: ${message}`);
  process.exitCode = 1;
});
