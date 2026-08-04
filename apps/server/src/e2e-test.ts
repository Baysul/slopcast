#!/usr/bin/env node
/**
 * End-to-End Test: Presenter -> Spectator Video Sharing Flow
 *
 * Validates the complete room-based screen sharing ecosystem:
 *   1. Parse slopcast.config.json for ports and endpoints
 *   2. Kill conflicting processes, spawn server + web dev servers
 *   3. Launch the Tauri presenter via WebdriverIO (embedded WebDriver):
 *      Wayland assertion, create room, preview + Go Live (MIGRATION §12)
 *   4. Launch Chromium spectator: join room, verify video stream
 *   5. Diagnostic validation: console logs, GPU probe report, stream health
 *   6. Graceful cleanup with retry-on-failure logic
 *
 * Prerequisites:
 *   Playwright + Chromium: pnpm add -D -w playwright && npx playwright install chromium
 *   Tauri e2e binary:      VITE_E2E=1 pnpm --filter desktop tauri build --features e2e
 *
 * Usage:
 *   pnpm tsx apps/server/src/e2e-test.ts
 */

import { type ChildProcess, execSync, spawn } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import path from 'node:path';

import { type AppConfig, loadConfig } from '@slopcast/shared-types/config';

import type { Browser, Page } from 'playwright';

/// GPU probe output (D5): replaces Electron's `app.getGPUInfo('complete')`.
interface GpuInfo {
  eglVendor: string | null;
  glRenderer: string | null;
  glVersion: string | null;
  softwareRasterizer: boolean;
}

/// Structured result of the WebdriverIO presenter phase (§12.2), written by
/// the spec to `presenter-phase.json` and read back by the harness.
interface PresenterPhase {
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

interface LogEntry {
  source: 'server' | 'web' | 'desktop-main' | 'desktop-renderer' | 'spectator' | 'livekit';
  message: string;
  timestamp: number;
}

interface TestResult {
  passed: boolean;
  roomCode: string;
  shareUrl: string;
  gpuReport: GpuInfo | null;
  consoleErrors: LogEntry[];
  spectatorConnected: boolean;
  spectatorVideoReceived: boolean;
  spectatorVideoPlaying: boolean;
  spectatorVideoWidth: number;
  spectatorVideoHeight: number;
  /** Continuous-frame check: two distinct requestVideoFrameCallback frames. */
  spectatorFramesFlowing: boolean;
  /** Pixel check: the decoded frame is not uniformly black. */
  spectatorFrameHasContent: boolean;
  /** Presenter-side native telemetry: published video frames, bytes, capture-pipeline pushes. */
  presenterVideoFlowing: boolean;
  presenterVideoFramesSent: number;
  presenterVideoBytesSent: number;
  captureFramesPushed: number;
  /** §9.1 preview emitter counter — proves base64 JPEG preview frames flowed. */
  previewFramesSent: number;
  decoderStallDetected: boolean;
  durationMs: number;
  retries: number;
  errors: string[];
}

const REPO_ROOT = path.resolve(__dirname, '..', '..', '..');
const OUTPUT_DIR = path.join(REPO_ROOT, 'test-output');
const DESKTOP_CONSOLE_LOG = path.join(OUTPUT_DIR, 'desktop-console.log');
const WEB_CONSOLE_LOG = path.join(OUTPUT_DIR, 'web-console.log');
const GPU_REPORT_PATH = path.join(OUTPUT_DIR, 'desktop-gpu-report.json');
const RESULT_PATH = path.join(OUTPUT_DIR, 'e2e-result.json');
/// Written by the harness to end the WDIO spec's hold step (§12.2); the
/// tauri-service then tears the app down at session end.
const PRESENTER_RELEASE_FLAG = path.join(OUTPUT_DIR, '.presenter-release');
const PRESENTER_PHASE_JSON = path.join(OUTPUT_DIR, 'presenter-phase.json');

const HEALTH_POLL_MS = 500;
const STARTUP_TIMEOUT_MS = 30_000;
const SPECTATOR_CONNECT_TIMEOUT_MS = 20_000;
const STREAM_TIMEOUT_MS = 20_000;

const FATAL_PATTERNS = [
  /(?:FATAL|fatal)\s*error/i,
  /uncaught\s*(?:exception|error)/i,
  /cannot read propert(?:y|ies) of (?:undefined|null)/i,
  /process\s*\.\s*exit/i,
  /segmentation\s*fault/i,
  /signal\s*:\s*SIG(?:SEGV|ABRT|ILL|FPE|BUS)/i,
  /ERR_MODULE_NOT_FOUND/,
  // Any failed subresource except the ubiquitous missing favicon.
  /Failed to load resource(?!.*favicon)/i,
  /WebSocket is closed before the connection is established/,
  /iceConnectionState.*failed/i,
  /GPU process.*crash/i,
  /Decoder stall confirmed/i,
  /framesDecoded=0.*codec=/i,
];

function log(prefix: string, msg: string): void {
  const ts = new Date().toISOString().slice(11, 23);
  process.stdout.write(`[${ts}] [${prefix}] ${msg}\n`);
}

function killPort(port: number): void {
  try {
    if (process.platform === 'linux') {
      execSync(`fuser -k ${port}/tcp 2>/dev/null || true`, { stdio: 'pipe' });
    } else if (process.platform === 'win32') {
      // No cross-platform-safe kill is available without extra tooling
      // (taskkill needs the exact PID); keep the netstat listing for diagnosis.
      execSync(`netstat -ano | findstr :${port}`, { stdio: 'pipe' });
    }
  } catch {
    log('CLEANUP', `Port ${port} was free`);
  }
}

function httpGet(url: string): Promise<number> {
  return new Promise((resolve, reject) => {
    const parsed = new URL(url);
    if (parsed.hostname === 'localhost') {
      parsed.hostname = '127.0.0.1';
    }
    const req = http
      .get(parsed.toString(), (res) => {
        res.resume();
        resolve(res.statusCode ?? 0);
      })
      .on('error', reject)
      .setTimeout(3000, () => {
        req.destroy();
        reject(new Error(`HTTP GET ${url} timed out`));
      });
  });
}

async function pollHealth(url: string, timeoutMs: number, label: string): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  log('HEALTH', `Polling ${label} at ${url} (timeout ${timeoutMs}ms)...`);
  while (Date.now() < deadline) {
    try {
      const status = await httpGet(url);
      if (status >= 200 && status < 400) {
        log('HEALTH', `${label} responded HTTP ${status}`);
        return;
      }
    } catch {
      log('HEALTH', `${label} not ready yet`);
    }
    await new Promise((r) => setTimeout(r, HEALTH_POLL_MS));
  }
  throw new Error(`${label} did not become healthy within ${timeoutMs}ms`);
}

function spawnLogging(command: string, args: string[], label: string, logEntries: LogEntry[]): ChildProcess {
  const proc = spawn(command, args, {
    cwd: REPO_ROOT,
    stdio: ['ignore', 'pipe', 'pipe'],
    shell: process.platform === 'win32',
    env: { ...process.env, FORCE_COLOR: '0' },
  });

  proc.stdout?.on('data', (data: Buffer) => {
    const lines = data.toString().split('\n').filter(Boolean);
    for (const line of lines) {
      logEntries.push({ source: label as LogEntry['source'], message: line, timestamp: Date.now() });
    }
  });

  proc.stderr?.on('data', (data: Buffer) => {
    const lines = data.toString().split('\n').filter(Boolean);
    for (const line of lines) {
      logEntries.push({ source: label as LogEntry['source'], message: line, timestamp: Date.now() });
    }
  });

  proc.on('error', (err) => {
    log('PROCESS', `${label} spawn error: ${err.message}`);
  });

  proc.on('exit', (code, sig) => {
    log('PROCESS', `${label} exited code=${code} signal=${sig}`);
  });

  return proc;
}

function tcpCheck(host: string, port: number, timeoutMs = 2000): Promise<boolean> {
  return new Promise((resolve) => {
    const socket = net.connect({ host, port });
    const done = (ok: boolean) => {
      socket.destroy();
      resolve(ok);
    };
    socket.once('connect', () => done(true));
    socket.once('error', () => done(false));
    socket.setTimeout(timeoutMs, () => done(false));
  });
}

function parseWsEndpoint(url: string): { host: string; port: number } | null {
  const m = url.match(/^wss?:\/\/([^/:]+)(?::(\d+))?/);
  if (!m) return null;
  return { host: m[1], port: m[2] ? Number.parseInt(m[2], 10) : 80 };
}

function findOnPath(bin: string): boolean {
  try {
    execSync(process.platform === 'win32' ? `where ${bin}` : `which ${bin}`, { stdio: 'pipe' });
    return true;
  } catch {
    return false;
  }
}

// The media plane needs a reachable LiveKit SFU. Without one the test can only
// fail at the streaming stage, so check early and start a local dev server
// when possible (the app defaults ws://localhost:7880 + devkey/secret match
// `livekit-server --dev` exactly).
async function ensureLiveKit(url: string, logEntries: LogEntry[]): Promise<ChildProcess | null> {
  const endpoint = parseWsEndpoint(url);
  if (!endpoint) {
    log('LIVEKIT', `Could not parse LiveKit URL "${url}" — assuming external SFU`);
    return null;
  }
  if (await tcpCheck(endpoint.host, endpoint.port)) {
    log('LIVEKIT', `LiveKit reachable at ${endpoint.host}:${endpoint.port}`);
    return null;
  }
  if (endpoint.host !== 'localhost' && endpoint.host !== '127.0.0.1') {
    throw new Error(
      `LiveKit SFU unreachable at ${url}. Check network/credentials (slopcast.config.json or LIVEKIT_URL).`,
    );
  }
  if (!findOnPath('livekit-server')) {
    throw new Error(
      `LiveKit SFU unreachable at ${url} and no livekit-server binary on PATH. ` +
        'Start one manually (livekit-server --dev) or point LIVEKIT_URL at a reachable instance.',
    );
  }
  log('LIVEKIT', `Nothing on ${endpoint.host}:${endpoint.port} — starting local livekit-server --dev...`);
  const proc = spawnLogging('livekit-server', ['--dev'], 'livekit', logEntries);
  const deadline = Date.now() + STARTUP_TIMEOUT_MS;
  while (Date.now() < deadline) {
    if (await tcpCheck(endpoint.host, endpoint.port)) {
      log('LIVEKIT', 'livekit-server is up');
      return proc;
    }
    await new Promise((r) => setTimeout(r, HEALTH_POLL_MS));
  }
  proc.kill('SIGTERM');
  throw new Error('livekit-server did not open its port within the startup timeout');
}

function findSpotifyProcess(): boolean {
  try {
    if (process.platform === 'linux') {
      const out = execSync('pgrep -x spotify 2>/dev/null || true', { encoding: 'utf-8' }).trim();
      return out.length > 0;
    }
    if (process.platform === 'win32') {
      const out = execSync('tasklist /FI "IMAGENAME eq Spotify.exe" 2>nul', { encoding: 'utf-8' });
      return out.includes('Spotify.exe');
    }
  } catch (err) {
    log('SPOTIFY', `Process detection failed: ${err}`);
  }
  return false;
}

function launchSpotify(): boolean {
  try {
    if (process.platform === 'linux') {
      execSync('nohup spotify >/dev/null 2>&1 &', { stdio: 'ignore', timeout: 3000 });
      return true;
    }
    if (process.platform === 'win32') {
      execSync('start "" "spotify:"', { stdio: 'ignore', timeout: 3000 });
      return true;
    }
  } catch (err) {
    log('SPOTIFY', `Failed to launch Spotify: ${err}`);
  }
  return false;
}

function validateLogs(entries: LogEntry[], label: string): LogEntry[] {
  const matched: LogEntry[] = [];

  for (const entry of entries) {
    for (const pattern of FATAL_PATTERNS) {
      if (pattern.test(entry.message)) {
        matched.push(entry);
        break;
      }
    }
  }

  if (matched.length > 0) {
    log('DIAGNOSTIC', `${label}: ${matched.length} suspicious log entry(s) detected`);
    for (const m of matched) {
      log('DIAGNOSTIC', `  [${m.source}] ${m.message.slice(0, 200)}`);
    }
  }

  return matched;
}

function validateGpuReport(report: GpuInfo | null): string[] {
  const issues: string[] = [];

  if (!report) {
    issues.push('GPU report is null — probe_gpu_info returned nothing');
    return issues;
  }

  if (!report.eglVendor) {
    issues.push('GPU probe reported no EGL vendor');
  }

  if (report.softwareRasterizer) {
    issues.push('GPU is software-rendered (llvmpipe/softpipe/SwiftShader)');
  }

  return issues;
}

interface ServerProcs {
  serverProc: ChildProcess | null;
  webProc: ChildProcess | null;
  livekitProc: ChildProcess | null;
}

async function ensureServers(config: AppConfig, logEntries: LogEntry[]): Promise<ServerProcs> {
  const procs: ServerProcs = { serverProc: null, webProc: null, livekitProc: null };

  procs.livekitProc = await ensureLiveKit(config.livekitUrl, logEntries);

  // Ensure API server is running.
  try {
    await pollHealth(`${config.apiEndpoint}/health`, 1000, 'API server');
    log('SPAWN', 'API server is already running and healthy');
  } catch {
    killPort(config.serverPort);
    await new Promise((r) => setTimeout(r, 500));
    log('SPAWN', 'Starting API server...');
    procs.serverProc = spawnLogging('pnpm', ['--filter', 'server', 'dev'], 'server', logEntries);
    await pollHealth(`${config.apiEndpoint}/health`, STARTUP_TIMEOUT_MS, 'API server');
  }

  // Ensure Web server is running.
  try {
    await pollHealth(config.websiteUrl, 1000, 'Web server');
    log('SPAWN', 'Web dev server is already running and healthy');
  } catch {
    killPort(config.webPort);
    await new Promise((r) => setTimeout(r, 500));
    log('SPAWN', 'Starting Web dev server...');
    procs.webProc = spawnLogging('pnpm', ['--filter', 'web', 'dev'], 'web', logEntries);
    await pollHealth(config.websiteUrl, STARTUP_TIMEOUT_MS, 'Web server');
  }

  // Spotify check.
  log('SPOTIFY', 'Checking Spotify process...');
  const spotifyRunning = findSpotifyProcess();
  if (spotifyRunning) {
    log('SPOTIFY', 'Spotify is already running');
  } else {
    log('SPOTIFY', 'Spotify not running — attempting to launch...');
    const launched = launchSpotify();
    if (launched) {
      log('SPOTIFY', 'Spotify launched successfully');
    } else {
      log('SPOTIFY', 'Could not launch Spotify (may not be installed)');
    }
  }

  return procs;
}

interface PresenterPhaseResult {
  phase: PresenterPhase;
  wdioProc: ChildProcess;
}

const PRESENTER_TIMEOUT_MS = 240_000;
const PRESENTER_TEARDOWN_MS = 30_000;

function writePresenterRelease(): void {
  writeFileSync(PRESENTER_RELEASE_FLAG, String(Date.now()));
}

async function waitForProcessExit(proc: ChildProcess, timeoutMs: number): Promise<void> {
  if (proc.exitCode !== null || proc.signalCode !== null) return;
  await new Promise<void>((resolve) => {
    const timer = setTimeout(() => resolve(), timeoutMs);
    proc.once('exit', () => {
      clearTimeout(timer);
      resolve();
    });
  });
}

/// Polls `presenter-phase.json` until the spec settles (ok or with errors) or
/// the wdio process exits early; returns `null` on timeout.
async function waitForPresenterPhase(wdioProc: ChildProcess, timeoutMs: number): Promise<PresenterPhase | null> {
  const deadline = Date.now() + timeoutMs;
  let phase: PresenterPhase | null = null;
  while (Date.now() < deadline) {
    if (existsSync(PRESENTER_PHASE_JSON)) {
      try {
        phase = JSON.parse(readFileSync(PRESENTER_PHASE_JSON, 'utf8')) as PresenterPhase;
        if (phase.ok || phase.errors.length > 0) break;
      } catch {
        // Partial write mid-step — keep polling.
      }
    }
    if (wdioProc.exitCode !== null || wdioProc.signalCode !== null) break;
    await new Promise((r) => setTimeout(r, 1000));
  }
  return phase;
}

/// Runs the presenter phase as a WebdriverIO subprocess against the Tauri
/// binary (embedded WebDriver, MIGRATION §12). The spec drives the UI,
/// samples telemetry and probes the GPU; the harness only orchestrates:
/// spawn, poll `presenter-phase.json`, then hand the room over to the
/// spectator phase. The spec's final test holds the session open until the
/// harness writes the release flag.
async function runPresenterPhase(
  config: AppConfig,
  logEntries: LogEntry[],
  result: TestResult,
): Promise<PresenterPhaseResult> {
  log('TEST', '=== Step 2: Presenter Automation (WebdriverIO + Tauri) ===');

  // Cargo workspace target dir lives at the repo root, not in src-tauri.
  const appBinary = path.join(REPO_ROOT, 'target', 'release', 'slopcast');
  if (!existsSync(appBinary)) {
    throw new Error(
      `Tauri e2e binary not found at ${appBinary}. ` +
        'Build it with: VITE_E2E=1 pnpm --filter desktop tauri build --features e2e ' +
        '(add --no-bundle when AppImage bundling is unavailable in the environment)',
    );
  }
  log('ELECTRON', `Launching Tauri app from ${appBinary}`);

  // Fresh handshake files: a stale release flag from a previous attempt would
  // end the spec's hold test immediately.
  rmSync(PRESENTER_RELEASE_FLAG, { force: true });
  rmSync(PRESENTER_PHASE_JSON, { force: true });

  // Isolate the app's config dir (stream-settings.json, onboarding state) so
  // persisted settings from a real session cannot leak into the test; the
  // embedded WebDriver env vars are added by the tauri-service itself.
  const wdioProc = spawn('pnpm', ['--filter', 'desktop', 'exec', 'wdio', 'run', './wdio.conf.ts'], {
    cwd: REPO_ROOT,
    stdio: ['ignore', 'pipe', 'pipe'],
    env: {
      ...process.env,
      NODE_ENV: 'test',
      XDG_CONFIG_HOME: path.join(REPO_ROOT, 'test-output', 'e2e-userdata'),
      // WebKitGTK compositing stability knob — streaming is unaffected.
      WEBKIT_DISABLE_DMABUF_RENDERER: '1',
      E2E_PHASE_JSON: PRESENTER_PHASE_JSON,
      E2E_RELEASE_FLAG: PRESENTER_RELEASE_FLAG,
      E2E_WEBSITE_URL: config.websiteUrl,
      FORCE_COLOR: '0',
    },
  });

  // The wdio output carries the tauri-service's forwarded backend logs and
  // the spec's own diagnostics — the renderer console is not forwarded by
  // WebKitGTK (R3), so failure detection leans on these + DOM assertions.
  const attachOutput = (stream: NodeJS.ReadableStream | null): void => {
    stream?.on('data', (data: Buffer) => {
      for (const line of data.toString().split('\n').filter(Boolean)) {
        logEntries.push({ source: 'desktop-main', message: line, timestamp: Date.now() });
      }
    });
  };
  attachOutput(wdioProc.stdout);
  attachOutput(wdioProc.stderr);
  wdioProc.on('error', (err) => {
    log('PROCESS', `wdio spawn error: ${err.message}`);
  });

  // Poll for the phase JSON; break on a settled result (ok or with errors) or
  // on an early wdio exit (binary/driver startup failure).
  const phase = await waitForPresenterPhase(wdioProc, PRESENTER_TIMEOUT_MS);

  if (!phase?.ok) {
    // End the spec's hold (or let a not-yet-started session exit) so the app
    // tears down before the retry.
    writePresenterRelease();
    const reason = phase
      ? phase.errors.join('; ') || 'no errors recorded'
      : 'no presenter-phase.json within timeout (wdio did not settle)';
    throw new Error(`Presenter phase failed: ${reason}`);
  }

  // The spec asserts the Wayland gate itself; the harness fails fast on the
  // phase result so a non-Wayland session never reaches the spectator.
  if (!phase.isWayland) {
    writePresenterRelease();
    throw new Error('Presenter phase: Wayland required — Slopcast is Wayland-only (D2)');
  }

  result.roomCode = phase.roomCode;
  result.shareUrl = phase.shareUrl;
  result.gpuReport = phase.gpuReport;
  result.presenterVideoFlowing = phase.telemetryFlowing;
  result.presenterVideoFramesSent = phase.videoFramesSent;
  result.presenterVideoBytesSent = phase.videoBytesSent;
  result.captureFramesPushed = phase.captureFramesPushed;
  result.previewFramesSent = phase.previewFramesSent;

  if (phase.gpuReport) {
    writeFileSync(GPU_REPORT_PATH, JSON.stringify(phase.gpuReport, null, 2));
    log('ELECTRON', `GPU report written to ${GPU_REPORT_PATH}`);
  }

  log('ELECTRON', `Room created: code=${phase.roomCode} url=${phase.shareUrl}`);
  log(
    'ELECTRON',
    `Presenter telemetry: framesSent=${phase.videoFramesSent} bytesSent=${phase.videoBytesSent} ` +
      `captureFramesPushed=${phase.captureFramesPushed} previewFramesSent=${phase.previewFramesSent} ` +
      `flowing=${phase.telemetryFlowing}`,
  );

  return { phase, wdioProc };
}

async function waitForSpectatorConnection(page: Page, result: TestResult): Promise<void> {
  try {
    await page.waitForFunction(
      () => {
        const badges = document.querySelectorAll('[role="status"]');
        for (const badge of badges) {
          const text = badge.textContent?.toLowerCase() ?? '';
          if (text.includes('live') || text.includes('connecting') || text.includes('waiting')) {
            return true;
          }
        }
        return false;
      },
      { timeout: SPECTATOR_CONNECT_TIMEOUT_MS },
    );
    log('SPECTATOR', 'Connection status badge visible');
    result.spectatorConnected = true;
  } catch (err) {
    log('SPECTATOR', `Connection status badge never appeared: ${err}`);
    result.errors.push('Spectator never reached connecting/live state');
  }
}

async function waitForSpectatorVideo(page: Page, result: TestResult): Promise<void> {
  try {
    await page.waitForSelector('video', { state: 'attached', timeout: STREAM_TIMEOUT_MS });

    // Poll for video frames — the element may appear before frames decode.
    await page.waitForFunction(
      () => {
        const videos = document.querySelectorAll('video');
        for (const video of videos) {
          if (video.videoWidth > 0 && video.videoHeight > 0 && !video.paused && video.readyState >= 2) {
            return true;
          }
        }
        return false;
      },
      { timeout: STREAM_TIMEOUT_MS },
    );

    const videoState = await page.evaluate(() => {
      const videos = document.querySelectorAll('video');
      for (const video of videos) {
        if (video.videoWidth > 0 && video.videoHeight > 0) {
          return {
            found: true,
            width: video.videoWidth,
            height: video.videoHeight,
            playing: !video.paused,
            readyState: video.readyState,
          };
        }
      }
      return { found: false, width: 0, height: 0, playing: false, readyState: -1 };
    });

    if (videoState.found) {
      log(
        'SPECTATOR',
        `Video streaming: ${videoState.width}x${videoState.height} playing=${videoState.playing} readyState=${videoState.readyState}`,
      );
      result.spectatorVideoReceived = true;
      result.spectatorVideoPlaying = videoState.playing;
      result.spectatorVideoWidth = videoState.width;
      result.spectatorVideoHeight = videoState.height;
    } else {
      log('SPECTATOR', 'Video element present but no video track data');
      result.errors.push('Video element found but no video frames received');
    }

    await checkSpectatorFrameFlow(page, result);
  } catch (err) {
    log('SPECTATOR', `Video element never appeared: ${err}`);
    result.errors.push('Spectator video element never appeared');
  }
}

/**
 * Malfunction check on the decoded stream: two `requestVideoFrameCallback`
 * frames ~1 s apart must arrive (continuous flow, not a single frame stall),
 * and the second frame's pixels must not be uniformly black (a dead capture
 * publishes black keepalive frames that satisfy the videoWidth check).
 */
async function checkSpectatorFrameFlow(page: Page, result: TestResult): Promise<void> {
  try {
    const frameCheck = await page.evaluate(async () => {
      const video = [...document.querySelectorAll('video')].find((v) => v.videoWidth > 0);
      if (!video) {
        throw new Error('no video element with frames');
      }
      const nextFrame = () =>
        new Promise<void>((resolve) => {
          video.requestVideoFrameCallback(() => resolve());
        });
      // Two consecutive decoded frames: the first could be a lone keepalive,
      // so a second callback proves the stream keeps flowing.
      await nextFrame();
      await nextFrame();

      const canvas = document.createElement('canvas');
      canvas.width = 64;
      canvas.height = 64;
      const ctx = canvas.getContext('2d', { willReadFrequently: true });
      if (!ctx) {
        throw new Error('canvas 2d context unavailable');
      }
      ctx.drawImage(video, 0, 0, 64, 64);
      const data = ctx.getImageData(0, 0, 64, 64).data;
      let nonBlack = 0;
      let varied = 0;
      for (let i = 0; i < data.length; i += 4) {
        const luma = 0.299 * data[i] + 0.587 * data[i + 1] + 0.114 * data[i + 2];
        if (luma > 16) nonBlack += 1;
        if (data[i] !== data[0] || data[i + 1] !== data[1] || data[i + 2] !== data[2]) varied += 1;
      }
      const pixels = data.length / 4;
      return {
        nonBlack: nonBlack / pixels,
        varied: varied / pixels,
        currentTime: video.currentTime,
      };
    });

    const hasContent = frameCheck.nonBlack > 0.3 || frameCheck.varied > 0.02;
    result.spectatorFramesFlowing = true;
    result.spectatorFrameHasContent = hasContent;
    log(
      'SPECTATOR',
      `Frame flow: two consecutive frames decoded, nonBlack=${frameCheck.nonBlack.toFixed(3)} ` +
        `varied=${frameCheck.varied.toFixed(3)} currentTime=${frameCheck.currentTime.toFixed(2)} ` +
        `content=${hasContent}`,
    );
  } catch (err) {
    log('SPECTATOR', `Frame flow check failed: ${err}`);
    result.errors.push(`Spectator frame flow check failed: ${String(err)}`);
  }
}

async function checkDecoderStall(page: Page, result: TestResult): Promise<void> {
  try {
    const stallHint = await page.evaluate(() => {
      const stallEl = document.querySelector('[data-decoder-stalled]');
      return stallEl ? stallEl.getAttribute('data-decoder-stalled') : null;
    });
    if (stallHint === 'true') {
      log('SPECTATOR', 'Decoder stall UI is visible — codec mismatch suspected');
      result.decoderStallDetected = true;
      result.errors.push('Spectator decoder stall detected (possible codec profile mismatch)');
    }
  } catch (err) {
    log('SPECTATOR', `Decoder stall check failed: ${err}`);
  }
}

async function runSpectatorPhase(logEntries: LogEntry[], result: TestResult): Promise<Browser> {
  log('TEST', '=== Step 3: Spectator Automation (Chromium) ===');

  const { chromium } = await import('playwright');

  const browser = await chromium.launch({
    args: [
      '--use-fake-ui-for-media-stream',
      '--use-fake-device-for-media-stream',
      '--autoplay-policy=no-user-gesture-required',
      '--no-sandbox',
      '--disable-setuid-sandbox',
    ],
  });

  const context = await browser.newContext({
    viewport: { width: 1280, height: 720 },
  });
  const spectatorPage = await context.newPage();

  spectatorPage.on('console', (msg) => {
    logEntries.push({
      source: 'spectator',
      message: `[${msg.type()}] ${msg.text()}`,
      timestamp: Date.now(),
    });
  });

  spectatorPage.on('pageerror', (err) => {
    logEntries.push({
      source: 'spectator',
      message: `UNCAUGHT: ${err.message}`,
      timestamp: Date.now(),
    });
  });

  log('SPECTATOR', `Navigating to ${result.shareUrl}`);
  // `networkidle` never settles with an active LiveKit WebSocket; wait for
  // the document and then poll for the connection state explicitly.
  await spectatorPage.goto(result.shareUrl, { waitUntil: 'domcontentloaded', timeout: SPECTATOR_CONNECT_TIMEOUT_MS });

  await waitForSpectatorConnection(spectatorPage, result);
  await waitForSpectatorVideo(spectatorPage, result);

  // Additional stability wait to let stream settle.
  await new Promise((r) => setTimeout(r, 3000));

  await checkDecoderStall(spectatorPage, result);

  return browser;
}

function validateDiagnostics(result: TestResult, logEntries: LogEntry[]): void {
  log('TEST', '=== Step 4: Diagnostic Validation ===');

  // Validate desktop logs.
  const desktopLogs = logEntries.filter((e) => e.source === 'desktop-main' || e.source === 'desktop-renderer');
  const desktopErrors = validateLogs(desktopLogs, 'Desktop');
  result.consoleErrors = desktopErrors;
  if (desktopErrors.length > 0) {
    result.errors.push(`${desktopErrors.length} suspicious desktop console log entry(s)`);
  }

  // Validate spectator logs.
  const spectatorLogs = logEntries.filter((e) => e.source === 'spectator');
  const spectatorErrors = validateLogs(spectatorLogs, 'Spectator');
  result.consoleErrors.push(...spectatorErrors);
  if (spectatorErrors.length > 0) {
    result.errors.push(`${spectatorErrors.length} suspicious spectator console log entry(s)`);
  }

  // Validate GPU report (probe_gpu_info, D5).
  const gpuIssues = validateGpuReport(result.gpuReport);
  for (const issue of gpuIssues) {
    result.errors.push(`GPU: ${issue}`);
  }

  // Validate spectator stream receipt.
  if (!result.spectatorVideoReceived) {
    result.errors.push('Spectator did not receive video stream within timeout');
  }

  // Validate that video frames keep flowing (not a single black keepalive).
  if (!result.spectatorFramesFlowing) {
    result.errors.push('Spectator video stalled after the first frame (no continuous frame flow)');
  }
  if (!result.spectatorFrameHasContent) {
    result.errors.push('Spectator video frames are uniformly black (capture malfunction)');
  }
  if (!result.presenterVideoFlowing) {
    result.errors.push('Presenter published no advancing video frames (videoFramesSent did not grow)');
  }
  if (result.previewFramesSent <= 0) {
    result.errors.push('Presenter emitted no preview frames (previewFramesSent stayed at 0)');
  }
}

function writeOutputArtifacts(logEntries: LogEntry[]): void {
  // Console logs are written on every outcome — they are the primary
  // diagnostic artifact when a step fails before validation runs.
  const consoleOutput = logEntries
    .map((e) => `[${new Date(e.timestamp).toISOString()}] [${e.source}] ${e.message}`)
    .join('\n');
  writeFileSync(DESKTOP_CONSOLE_LOG, consoleOutput);
  const webLogEntries = logEntries.filter((e) => e.source === 'spectator');
  writeFileSync(WEB_CONSOLE_LOG, webLogEntries.map((e) => e.message).join('\n'));
}

async function shutdownResources(
  browser: Browser | null,
  wdioProc: ChildProcess | null,
  procs: ServerProcs,
  config: AppConfig,
): Promise<void> {
  log('CLEANUP', 'Shutting down...');
  if (browser) {
    await browser.close().catch(() => log('CLEANUP', 'Spectator browser already closed'));
  }
  // Release the presenter spec's hold first so the wdio session (and with it
  // the Tauri app) tears down gracefully instead of being SIGKILLed.
  if (wdioProc) {
    writePresenterRelease();
    await waitForProcessExit(wdioProc, PRESENTER_TEARDOWN_MS);
    if (wdioProc.exitCode === null && wdioProc.signalCode === null) {
      log('CLEANUP', 'wdio did not exit after release — killing');
      wdioProc.kill('SIGTERM');
      await waitForProcessExit(wdioProc, 5000);
    }
  }
  for (const proc of [procs.serverProc, procs.webProc, procs.livekitProc]) {
    try {
      proc?.kill('SIGTERM');
    } catch (err) {
      log('CLEANUP', `Process kill failed: ${err}`);
    }
  }

  // Ensure ports are freed.
  killPort(config.serverPort);
  killPort(config.webPort);
}

async function runTest(): Promise<TestResult> {
  const startTime = Date.now();
  const result: TestResult = {
    passed: false,
    roomCode: '',
    shareUrl: '',
    gpuReport: null,
    consoleErrors: [],
    spectatorConnected: false,
    spectatorVideoReceived: false,
    spectatorVideoPlaying: false,
    spectatorVideoWidth: 0,
    spectatorVideoHeight: 0,
    spectatorFramesFlowing: false,
    spectatorFrameHasContent: false,
    presenterVideoFlowing: false,
    presenterVideoFramesSent: 0,
    presenterVideoBytesSent: 0,
    captureFramesPushed: 0,
    previewFramesSent: 0,
    decoderStallDetected: false,
    durationMs: 0,
    retries: 0,
    errors: [],
  };

  const logEntries: LogEntry[] = [];

  log('TEST', '=== Step 1: Configuration & Environment Setup ===');

  const config = loadConfig();
  log('CONFIG', `serverPort=${config.serverPort} webPort=${config.webPort}`);
  log('CONFIG', `apiEndpoint=${config.apiEndpoint} websiteUrl=${config.websiteUrl}`);

  mkdirSync(OUTPUT_DIR, { recursive: true });

  // Resources tracked for guaranteed cleanup — a failure at any step must not
  // leak server processes, browser instances or the presenter app into the
  // next retry.
  let browser: Browser | null = null;
  let wdioProc: ChildProcess | null = null;
  let procs: ServerProcs = { serverProc: null, webProc: null, livekitProc: null };

  try {
    procs = await ensureServers(config, logEntries);

    const presenter = await runPresenterPhase(config, logEntries, result);
    wdioProc = presenter.wdioProc;

    browser = await runSpectatorPhase(logEntries, result);

    validateDiagnostics(result, logEntries);
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    log('TEST', `FATAL: ${message}`);
    result.errors.push(message);
  } finally {
    await shutdownResources(browser, wdioProc, procs, config);
    writeOutputArtifacts(logEntries);
  }

  result.passed = result.errors.length === 0;
  result.durationMs = Date.now() - startTime;

  writeFileSync(RESULT_PATH, JSON.stringify(result, null, 2));
  log('TEST', `Result written to ${RESULT_PATH}`);

  return result;
}

const RETRY_DELAY_MS = 3000;

const waitBeforeRetry = (attempt: number, maxRetries: number): Promise<void> => {
  if (attempt >= maxRetries) return Promise.resolve();
  log('TEST', 'Retrying in 3 seconds...');
  return new Promise((r) => setTimeout(r, RETRY_DELAY_MS));
};

const reportFinalFailure = (lastResult: TestResult | null): void => {
  if (lastResult) {
    log('TEST', 'All retries exhausted. Final result: FAILED');
    log('TEST', `Root cause summary: ${lastResult.errors.join('; ') || 'Unknown failure'}`);
  } else {
    log('TEST', 'All retries exhausted. No result produced.');
  }
};

async function main(): Promise<void> {
  const maxRetries = 2;
  let lastResult: TestResult | null = null;

  for (let attempt = 1; attempt <= maxRetries; attempt++) {
    log('TEST', `========================================`);
    log('TEST', `Attempt ${attempt} of ${maxRetries}`);
    log('TEST', `========================================`);

    try {
      // No outer race: every wait inside runTest is individually bounded, and
      // an orphaned run's cleanup would kill the next attempt's servers.
      const result = await runTest();

      result.retries = attempt - 1;
      lastResult = result;

      if (result.passed) {
        log('TEST', 'RESULT: PASSED');
        printSummary(result);
        process.exit(0);
      }

      log('TEST', `RESULT: FAILED (${result.errors.length} error(s))`);
      printSummary(result);
      await waitBeforeRetry(attempt, maxRetries);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      log('TEST', `FATAL: ${message}`);
      await waitBeforeRetry(attempt, maxRetries);
    }
  }

  // All retries exhausted.
  reportFinalFailure(lastResult);
  process.exit(1);
}

function printSummary(result: TestResult): void {
  log('SUMMARY', '============================================');
  log('SUMMARY', `Room Code:       ${result.roomCode || 'N/A'}`);
  log('SUMMARY', `Share URL:       ${result.shareUrl || 'N/A'}`);
  log('SUMMARY', `Duration:        ${(result.durationMs / 1000).toFixed(1)}s`);
  log('SUMMARY', `Retries:         ${result.retries}`);
  log('SUMMARY', `Spec Connected:  ${result.spectatorConnected}`);
  log('SUMMARY', `Video Received:  ${result.spectatorVideoReceived}`);
  log('SUMMARY', `Video Playing:   ${result.spectatorVideoPlaying}`);
  log('SUMMARY', `Video Size:      ${result.spectatorVideoWidth}x${result.spectatorVideoHeight}`);
  log('SUMMARY', `Decoder Stall:   ${result.decoderStallDetected}`);
  log('SUMMARY', `GPU:             ${result.gpuReport ? 'Probed' : 'Missing'}`);
  log('SUMMARY', `Preview Frames:  ${result.previewFramesSent}`);
  log('SUMMARY', `Console Errors:  ${result.consoleErrors.length}`);
  log('SUMMARY', `Errors:          ${result.errors.length}`);

  if (result.errors.length > 0) {
    log('SUMMARY', '--- Error Details ---');
    for (const err of result.errors) {
      log('SUMMARY', `  • ${err}`);
    }
  }

  if (result.consoleErrors.length > 0) {
    log('SUMMARY', '--- Console Error Samples ---');
    for (const err of result.consoleErrors.slice(0, 5)) {
      log('SUMMARY', `  [${err.source}] ${err.message.slice(0, 150)}`);
    }
  }

  log('SUMMARY', '============================================');
}

main().catch((err) => {
  console.error('Unhandled error in e2e-test:', err);
  process.exit(1);
});
