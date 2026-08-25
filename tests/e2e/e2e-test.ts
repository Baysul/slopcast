#!/usr/bin/env node

import { type ChildProcess, execSync, spawn } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import path from 'node:path';

import { isResolutionPreset, RESOLUTION_DIMENSIONS, type ResolutionPreset } from '@slopcast/shared-types';
import { type AppConfig, loadConfig } from '@slopcast/shared-types/config';
import { RoomServiceClient } from 'livekit-server-sdk';

import type { Browser, Page } from 'playwright';

const parseResolution = (value: string): ResolutionPreset => {
  if (!isResolutionPreset(value)) throw new Error(`Unsupported E2E resolution: ${value}`);
  return value;
};

const passFps = Number(process.env.E2E_FPS ?? 60);
const passBitrate = Number(process.env.E2E_BITRATE_LIMIT ?? 20_000_000);
const passResolution = parseResolution(process.env.E2E_RESOLUTION ?? '1080p');

const passBitrateFor = (codec: string): number => (codec === 'av1' ? 8_000_000 : passBitrate);

interface GpuInfo {
  eglVendor: string | null;
  glRenderer: string | null;
  glVersion: string | null;
  softwareRasterizer: boolean;
}

async function logRoomPublications(config: AppConfig, roomCode: string): Promise<void> {
  const host = config.livekitUrl.replace(/^ws(s?):\/\//, 'http$1://');
  const roomClient = new RoomServiceClient(host, config.livekitApiKey, config.livekitApiSecret);
  const deadline = Date.now() + 10_000;
  let summary: Array<{ identity: string; tracks: Array<{ sid: string; mimeType: string }> }> = [];

  while (Date.now() < deadline) {
    const participants = await roomClient.listParticipants(roomCode);
    summary = participants.map((participant) => ({
      identity: participant.identity,
      tracks: participant.tracks.map((track) => ({ sid: track.sid, mimeType: track.mimeType })),
    }));
    if (summary.some((participant) => participant.tracks.some((track) => track.mimeType.startsWith('video/')))) {
      log('LIVEKIT', `Room publications: ${JSON.stringify(summary)}`);
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }

  throw new Error(`Presenter video publication did not appear in LiveKit: ${JSON.stringify(summary)}`);
}

interface PresenterPhase {
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
  spectatorCodec: string | null;
  spectatorDecodedFps: number;
  spectatorFramesFlowing: boolean;
  spectatorFrameHasContent: boolean;
  spectatorNotifiedOfStop: boolean;
  presenterVideoFlowing: boolean;
  presenterVideoFramesEncoded: number;
  presenterVideoBytesSent: number;
  presenterTelemetryFps: number;
  presenterBitrateBps: number;
  captureFramesPushed: number;
  previewFramesSent: number;
  videoCodecReported: string | null;
  encoderImplementation: string | null;
  decoderStallDetected: boolean;
  codecsTested: string[];
  codecResults: Record<
    string,
    {
      passed: boolean;
      errors: string[];
      encoderImplementation: string | null;
      videoCodecReported: string | null;
      presenterBitrateBps: number;
      presenterTelemetryFps: number;
      spectatorDecodedFps: number;
      spectatorVideoWidth: number;
      spectatorVideoHeight: number;
      spectatorCodec: string | null;
    }
  >;
  durationMs: number;
  retries: number;
  errors: string[];
}

const REPO_ROOT = path.resolve(import.meta.dirname, '..', '..');
const OUTPUT_DIR = path.join(REPO_ROOT, 'test-output');
const DESKTOP_CONSOLE_LOG = path.join(OUTPUT_DIR, 'desktop-console.log');
const WEB_CONSOLE_LOG = path.join(OUTPUT_DIR, 'web-console.log');
const GPU_REPORT_PATH = path.join(OUTPUT_DIR, 'desktop-gpu-report.json');
const RESULT_PATH = path.join(OUTPUT_DIR, 'e2e-result.json');
const PRESENTER_RELEASE_FLAG = path.join(OUTPUT_DIR, '.presenter-release');
const PRESENTER_PHASE_JSON = path.join(OUTPUT_DIR, 'presenter-phase.json');
const PRESENTER_STOP_FLAG = path.join(OUTPUT_DIR, '.presenter-stop-request');
const PRESENTER_STOPPED_FLAG = path.join(OUTPUT_DIR, '.presenter-stopped');
const PRESENTER_SPECTATOR_READY_FLAG = path.join(OUTPUT_DIR, '.spectator-ready');

const HEALTH_POLL_MS = 500;
const STARTUP_TIMEOUT_MS = 30_000;
const SPECTATOR_CONNECT_TIMEOUT_MS = 20_000;
const PRESENTER_STOP_TIMEOUT_MS = 60_000;
const STREAM_END_TIMEOUT_MS = 30_000;
const STREAM_TIMEOUT_MS = 20_000;

const FATAL_PATTERNS = [
  /(?:FATAL|fatal)\s*error/i,
  /uncaught\s*(?:exception|error)/i,
  /cannot read propert(?:y|ies) of (?:undefined|null)/i,
  /process\s*\.\s*exit/i,
  /segmentation\s*fault/i,
  /signal\s*:\s*SIG(?:SEGV|ABRT|ILL|FPE|BUS)/i,
  /ERR_MODULE_NOT_FOUND/,
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
      execSync(`netstat -ano | findstr :${port}`, { stdio: 'pipe' });
    }
  } catch {
    log('CLEANUP', `Port ${port} was free`);
  }
}

function killStraySlopcast(): void {
  try {
    if (process.platform === 'linux') {
      execSync(`pkill -f 'target/release/slopcast' 2>/dev/null || true`, { stdio: 'pipe' });
    } else if (process.platform === 'win32') {
      execSync('taskkill /IM slopcast.exe /F >nul 2>&1 || exit /b 0', { stdio: 'pipe' });
    }
  } catch {
    log('CLEANUP', 'No stray app processes to kill');
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

function spawnLogging(
  command: string,
  args: string[],
  label: LogEntry['source'],
  logEntries: LogEntry[],
): ChildProcess {
  const proc = spawn(command, args, {
    cwd: REPO_ROOT,
    stdio: ['ignore', 'pipe', 'pipe'],
    shell: process.platform === 'win32',
    env: { ...process.env, FORCE_COLOR: '0' },
  });

  proc.stdout?.on('data', (data: Buffer) => {
    const lines = data.toString().split('\n').filter(Boolean);
    for (const line of lines) {
      logEntries.push({ source: label, message: line, timestamp: Date.now() });
    }
  });

  proc.stderr?.on('data', (data: Buffer) => {
    const lines = data.toString().split('\n').filter(Boolean);
    for (const line of lines) {
      logEntries.push({ source: label, message: line, timestamp: Date.now() });
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

  const host = m[1];
  if (!host) return null;

  return { host, port: m[2] ? Number.parseInt(m[2], 10) : 80 };
}

function findOnPath(bin: string): boolean {
  try {
    execSync(process.platform === 'win32' ? `where ${bin}` : `which ${bin}`, { stdio: 'pipe' });
    return true;
  } catch {
    return false;
  }
}

async function ensureLiveKit(url: string, logEntries: LogEntry[]): Promise<ChildProcess | null> {
  const endpoint = parseWsEndpoint(url);
  if (!endpoint) {
    log('LIVEKIT', `Could not parse LiveKit URL "${url}" — assuming external SFU`);
    return null;
  }
  if (endpoint.host !== 'localhost' && endpoint.host !== '127.0.0.1' && endpoint.host !== '::1') {
    if (await tcpCheck(endpoint.host, endpoint.port)) {
      log('LIVEKIT', `LiveKit reachable at ${endpoint.host}:${endpoint.port}`);
      return null;
    }
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
  for (const port of [endpoint.port, 7881, 7882]) {
    killPort(port);
  }
  await new Promise((r) => setTimeout(r, 500));
  log('LIVEKIT', `Starting native livekit-server --dev on :${endpoint.port}...`);
  const proc = spawnLogging(
    'livekit-server',
    ['--dev', '--bind', '0.0.0.0', '--port', String(endpoint.port)],
    'livekit',
    logEntries,
  );
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
  presenterProc: ChildProcess;
}

const PRESENTER_TIMEOUT_MS = 240_000;
const PRESENTER_TEARDOWN_MS = 30_000;

function writePresenterRelease(): void {
  writeFileSync(PRESENTER_RELEASE_FLAG, String(Date.now()));
}

async function waitForProcessExit(proc: ChildProcess, timeoutMs: number): Promise<void> {
  const outputClosed = [proc.stdout, proc.stderr].every(
    (stream) => stream == null || stream.readableEnded || stream.destroyed,
  );
  if ((proc.exitCode !== null || proc.signalCode !== null) && outputClosed) return;
  await new Promise<void>((resolve) => {
    const timer = setTimeout(() => resolve(), timeoutMs);
    proc.once('close', () => {
      clearTimeout(timer);
      resolve();
    });
  });
}

async function waitForPresenterPhase(presenterProc: ChildProcess, timeoutMs: number): Promise<PresenterPhase | null> {
  const deadline = Date.now() + timeoutMs;
  let phase: PresenterPhase | null = null;
  while (Date.now() < deadline) {
    if (existsSync(PRESENTER_PHASE_JSON)) {
      try {
        // SAFETY: the presenter writes this file from the PresenterPhase object in its Playwright script.
        phase = JSON.parse(readFileSync(PRESENTER_PHASE_JSON, 'utf8')) as PresenterPhase;
        if (phase.handoffReady || phase.errors.length > 0) break;
      } catch (error) {
        console.debug('[e2e] presenter phase is not complete:', error);
      }
    }
    if (presenterProc.exitCode !== null || presenterProc.signalCode !== null) break;
    await new Promise((r) => setTimeout(r, 1000));
  }
  return phase;
}

async function runPresenterPhase(
  config: AppConfig,
  logEntries: LogEntry[],
  result: TestResult,
  codec: string,
  captureMode: string,
  ownProcess: (process: ChildProcess) => void,
): Promise<PresenterPhaseResult> {
  log('TEST', `=== Step 2: Presenter Automation (Playwright CDP + Tauri, codec=${codec}) ===`);

  const appBinary = process.env.E2E_APP_BINARY_PATH ?? path.join(REPO_ROOT, 'target', 'release', 'slopcast');
  if (!existsSync(appBinary)) {
    throw new Error(
      `Tauri e2e binary not found at ${appBinary}. ` +
        'Build it with: VITE_E2E=1 pnpm --filter desktop tauri build --features e2e ' +
        '(add --no-bundle when AppImage bundling is unavailable in the environment)',
    );
  }
  log('TAURI', `Launching Tauri app from ${appBinary}`);

  rmSync(PRESENTER_RELEASE_FLAG, { force: true });
  rmSync(PRESENTER_PHASE_JSON, { force: true });
  rmSync(PRESENTER_STOP_FLAG, { force: true });
  rmSync(PRESENTER_STOPPED_FLAG, { force: true });
  rmSync(PRESENTER_SPECTATOR_READY_FLAG, { force: true });
  killStraySlopcast();

  const sharedEnv = {
    ...process.env,
    NODE_ENV: 'test',
    XDG_CONFIG_HOME: path.join(REPO_ROOT, 'test-output', 'e2e-userdata'),
    SLOPCAST_E2E_CAPTURE: captureMode === 'portal' ? '' : 'synthetic',
  };
  const appProc = spawn(appBinary, [], {
    cwd: path.dirname(appBinary),
    stdio: ['ignore', 'pipe', 'pipe'],
    env: sharedEnv,
  });
  ownProcess(appProc);
  const attachOutput = (stream: NodeJS.ReadableStream): void => {
    stream.on('data', (data: Buffer) => {
      for (const line of data.toString().split('\n').filter(Boolean)) {
        logEntries.push({ source: 'desktop-main', message: line, timestamp: Date.now() });
      }
    });
  };
  if (appProc.stdout) attachOutput(appProc.stdout);
  if (appProc.stderr) attachOutput(appProc.stderr);
  appProc.on('error', (err) => {
    log('PROCESS', `presenter binary spawn error: ${err.message}`);
  });

  const presenterProc = spawn(
    'pnpm',
    ['--filter', 'desktop', 'exec', 'node', '../../tests/e2e/desktop/presenter.playwright.ts'],
    {
      cwd: REPO_ROOT,
      stdio: ['ignore', 'pipe', 'pipe'],
      env: {
        ...sharedEnv,
        E2E_PHASE_JSON: PRESENTER_PHASE_JSON,
        E2E_RELEASE_FLAG: PRESENTER_RELEASE_FLAG,
        E2E_STOP_FLAG: PRESENTER_STOP_FLAG,
        E2E_STOPPED_FLAG: PRESENTER_STOPPED_FLAG,
        E2E_SPECTATOR_READY_FLAG: PRESENTER_SPECTATOR_READY_FLAG,
        E2E_WEBSITE_URL: config.websiteUrl,
        E2E_CODEC: codec,
        E2E_EXPECTED_FPS: String(passFps),
        E2E_EXPECTED_BITRATE: String(passBitrateFor(codec)),
        E2E_CAPTURE: captureMode,
        FORCE_COLOR: '0',
      },
    },
  );
  ownProcess(presenterProc);

  attachOutput(presenterProc.stdout);
  attachOutput(presenterProc.stderr);
  presenterProc.on('error', (err) => {
    log('PROCESS', `presenter script spawn error: ${err.message}`);
  });

  const phase = await waitForPresenterPhase(presenterProc, PRESENTER_TIMEOUT_MS);

  if (!phase?.ok) {
    writePresenterRelease();
    const reason = phase
      ? phase.errors.join('; ') || 'no errors recorded'
      : 'no presenter-phase.json within timeout (presenter script did not settle)';
    throw new Error(`Presenter phase failed: ${reason}`);
  }

  if (!phase.handoffReady || phase.roomCode.length === 0) {
    writePresenterRelease();
    throw new Error(
      `Presenter phase never went live (handoffReady=${phase.handoffReady}, roomCode="${phase.roomCode}")`,
    );
  }

  if (!phase.isWayland && captureMode === 'portal') {
    writePresenterRelease();
    throw new Error('Presenter phase: Wayland required — Slopcast is Wayland-only (D2)');
  }

  result.roomCode = phase.roomCode;
  result.shareUrl = phase.shareUrl;
  result.gpuReport = phase.gpuReport;
  result.presenterVideoFlowing = phase.telemetryFlowing;
  result.presenterVideoFramesEncoded = phase.videoFramesEncoded;
  result.presenterVideoBytesSent = phase.videoBytesSent;
  result.presenterTelemetryFps = phase.telemetryFps;
  result.captureFramesPushed = phase.captureFramesPushed;
  result.previewFramesSent = phase.previewFramesSent;
  result.videoCodecReported = phase.videoCodecReported;
  result.encoderImplementation = phase.encoderImplementation;

  if (phase.gpuReport) {
    writeFileSync(GPU_REPORT_PATH, JSON.stringify(phase.gpuReport, null, 2));
    log('TAURI', `GPU report written to ${GPU_REPORT_PATH}`);
  }

  log('TAURI', `Room created: code=${phase.roomCode} url=${phase.shareUrl}`);
  log(
    'TAURI',
    `Presenter telemetry: framesEncoded=${phase.videoFramesEncoded} bytesSent=${phase.videoBytesSent} ` +
      `captureFramesPushed=${phase.captureFramesPushed} previewFramesSent=${phase.previewFramesSent} ` +
      `flowing=${phase.telemetryFlowing}`,
  );

  return { phase, presenterProc };
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
      undefined,
      { timeout: SPECTATOR_CONNECT_TIMEOUT_MS },
    );
    log('SPECTATOR', 'Connection status badge visible');
    result.spectatorConnected = true;
  } catch (err) {
    log('SPECTATOR', `Connection status badge never appeared: ${err}`);
    result.errors.push('Spectator never reached connecting/live state');
  }
}

async function waitForSpectatorVideo(
  page: Page,
  result: TestResult,
  codec: string,
  captureMode: string,
): Promise<void> {
  try {
    await page.waitForSelector('video', { state: 'attached', timeout: STREAM_TIMEOUT_MS });

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
      undefined,
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
      const expectedDims = RESOLUTION_DIMENSIONS[passResolution] ?? RESOLUTION_DIMENSIONS['720p'];
      if (
        captureMode === 'synthetic' &&
        (videoState.width !== expectedDims.width || videoState.height !== expectedDims.height)
      ) {
        result.errors.push(
          `Spectator received ${videoState.width}x${videoState.height}, expected the published ${expectedDims.width}x${expectedDims.height} (single layer)`,
        );
      }
    } else {
      log('SPECTATOR', 'Video element present but no video track data');
      result.errors.push('Video element found but no video frames received');
    }

    await checkSpectatorFrameFlow(page, result);
    await checkSpectatorDecodedFps(page, result, codec);
  } catch (err) {
    log('SPECTATOR', `Video element never appeared: ${err}`);
    result.errors.push('Spectator video element never appeared');
  }
}

async function checkSpectatorFrameFlow(page: Page, result: TestResult): Promise<void> {
  try {
    const frameCheck = await page.evaluate(async () => {
      const video = [...document.querySelectorAll('video')].find((v) => v.videoWidth > 0);
      if (!video) {
        throw new Error('no video element with frames');
      }
      await Promise.race([
        new Promise<void>((resolve) => video.requestVideoFrameCallback(() => resolve())),
        new Promise<never>((_, reject) => setTimeout(() => reject(new Error('first video frame timed out')), 5000)),
      ]);
      await Promise.race([
        new Promise<void>((resolve) => video.requestVideoFrameCallback(() => resolve())),
        new Promise<never>((_, reject) => setTimeout(() => reject(new Error('second video frame timed out')), 5000)),
      ]);

      const canvas = document.createElement('canvas');
      canvas.width = 64;
      canvas.height = 64;
      const ctx = canvas.getContext('2d', { willReadFrequently: true });
      if (!ctx) {
        throw new Error('canvas 2d context unavailable');
      }
      ctx.drawImage(video, 0, 0, 64, 64);
      const data = ctx.getImageData(0, 0, 64, 64).data;
      const pixels = data.length / 4;
      const [firstRed = 0, firstGreen = 0, firstBlue = 0] = data;
      let nonBlackCount = 0;
      let variedCount = 0;
      for (let i = 0; i < data.length; i += 4) {
        const [red = 0, green = 0, blue = 0] = data.subarray(i, i + 3);
        const luma = 0.299 * red + 0.587 * green + 0.114 * blue;
        if (luma > 16) nonBlackCount += 1;
        if (red !== firstRed || green !== firstGreen || blue !== firstBlue) variedCount += 1;
      }
      return {
        nonBlack: nonBlackCount / pixels,
        varied: variedCount / pixels,
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

async function enableSpectatorTelemetry(page: Page): Promise<void> {
  await page.locator('video').first().hover();
  await page.getByRole('button', { name: 'Player settings' }).click();
  const telemetryToggle = page.getByRole('switch', { name: 'Show telemetry' });
  if (!(await telemetryToggle.isChecked())) {
    await telemetryToggle.click();
  }
  await page.keyboard.press('Escape');
}

async function checkSpectatorDecodedFps(page: Page, result: TestResult, codec: string): Promise<void> {
  try {
    await enableSpectatorTelemetry(page);

    const fpsValue = page.locator('[data-testid="spectator-telemetry-fps"]');
    await page.waitForFunction(
      () => {
        const text = document.querySelector('[data-testid="spectator-telemetry-fps"]')?.textContent;
        return text != null && Number.parseFloat(text) > 0;
      },
      undefined,
      { timeout: 8000 },
    );
    const spectatorCodec = (await page.locator('[data-testid="spectator-telemetry-codec"]').textContent())?.trim();
    const expectedCodec = codecLabelForTest(codec);
    result.spectatorCodec = spectatorCodec || null;
    if (!spectatorCodec || spectatorCodec.toUpperCase() !== expectedCodec.toUpperCase()) {
      result.errors.push(
        `Spectator codec mismatch: requested ${expectedCodec}, receiver reported ${spectatorCodec || 'none'}`,
      );
    }

    const samples: number[] = [];
    for (let sampleIndex = 0; sampleIndex < 3; sampleIndex++) {
      if (sampleIndex > 0) {
        await new Promise((resolve) => setTimeout(resolve, 2200));
      }
      const fps = Number.parseFloat((await fpsValue.textContent()) ?? '');
      if (Number.isFinite(fps) && fps > 0) samples.push(fps);
    }
    if (samples.length !== 3) {
      throw new Error(`received only ${samples.length} valid decoded-FPS telemetry sample(s)`);
    }

    samples.sort((left, right) => left - right);
    result.spectatorDecodedFps = samples[1] ?? 0;
    const minimumFps = Math.floor(passFps * 0.8);
    if (result.spectatorDecodedFps < minimumFps) {
      result.errors.push(
        `Spectator decoded ${result.spectatorDecodedFps.toFixed(1)} fps, expected at least ${minimumFps} fps for a configured ${passFps} fps stream`,
      );
    }
    log(
      'SPECTATOR',
      `Decoded FPS samples=${samples.join(',')} median=${result.spectatorDecodedFps.toFixed(1)} ` +
        `minimum=${minimumFps}`,
    );
  } catch (err) {
    log('SPECTATOR', `Decoded FPS check failed: ${err}`);
    result.errors.push(`Spectator decoded FPS check failed: ${String(err)}`);
  }
}

function codecLabelForTest(codec: string): string {
  return codec.replace(/^H26[45]$/i, (m) => `H.${m.slice(1)}`);
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

const waitForFile = async (filePath: string, timeoutMs: number): Promise<boolean> => {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (existsSync(filePath)) return true;
    await new Promise((r) => setTimeout(r, 500));
  }
  return false;
};

async function runPresenterStopRoundTrip(page: Page, result: TestResult): Promise<void> {
  log('TEST', '=== Presenter stop round-trip (spectator must be informed) ===');
  rmSync(PRESENTER_STOPPED_FLAG, { force: true });
  writeFileSync(PRESENTER_STOP_FLAG, 'stop');
  const stopped = await waitForFile(PRESENTER_STOPPED_FLAG, PRESENTER_STOP_TIMEOUT_MS);
  if (!stopped) {
    log('TEST', 'Presenter never acknowledged the stop request');
    result.errors.push('Presenter never stopped on request (stopped flag not written)');
    return;
  }
  try {
    await page.waitForFunction(
      () => {
        const badges = document.querySelectorAll('[role="status"]');
        for (const badge of badges) {
          const text = badge.textContent?.toLowerCase() ?? '';
          if (text.includes('stream ended') || text.includes('presenter left')) {
            return true;
          }
        }
        return false;
      },
      undefined,
      { timeout: STREAM_END_TIMEOUT_MS },
    );
    result.spectatorNotifiedOfStop = true;
    log('SPECTATOR', 'Badge reports the stream ended — stop propagated to the spectator');
  } catch (err) {
    log('SPECTATOR', `Badge never reported the stream ended: ${err}`);
    result.errors.push('Spectator was not informed when the presenter stopped (badge never left Live)');
  }
}

async function runSpectatorPhase(
  logEntries: LogEntry[],
  result: TestResult,
  codec: string,
  captureMode: string,
): Promise<Browser> {
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
  let shouldCloseBrowser = true;

  try {
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
    await spectatorPage.goto(result.shareUrl, {
      waitUntil: 'domcontentloaded',
      timeout: SPECTATOR_CONNECT_TIMEOUT_MS,
    });

    await waitForSpectatorConnection(spectatorPage, result);

    if (codec === 'h265') {
      log('SPECTATOR', 'H.265 decode checks skipped — headless Chromium has no HEVC decoder');
      await new Promise((r) => setTimeout(r, 3000));
    } else {
      await waitForSpectatorVideo(spectatorPage, result, codec, captureMode);

      await new Promise((r) => setTimeout(r, 3000));

      await checkDecoderStall(spectatorPage, result);
    }

    writeFileSync(PRESENTER_SPECTATOR_READY_FLAG, 'ready');
    const telemetryDeadline = Date.now() + 20_000;
    while (Date.now() < telemetryDeadline) {
      let phase: PresenterPhase;
      try {
        // SAFETY: the presenter writes this file from the PresenterPhase object in its Playwright script.
        phase = JSON.parse(readFileSync(PRESENTER_PHASE_JSON, 'utf8')) as PresenterPhase;
      } catch {
        await new Promise((resolve) => setTimeout(resolve, 100));
        continue;
      }
      if (phase.errors.length > 0) {
        result.errors.push(...phase.errors.map((error) => `Presenter post-subscription telemetry: ${error}`));
        break;
      }
      if (phase.postSubscriptionTelemetryReady) {
        result.presenterVideoBytesSent = phase.videoBytesSent;
        result.presenterBitrateBps = phase.senderBitrateBps;
        break;
      }
      await new Promise((resolve) => setTimeout(resolve, 500));
    }
    if (result.presenterVideoBytesSent <= 0) {
      result.errors.push('Presenter telemetry reported no RTP bytes after the spectator subscribed');
    }

    await runPresenterStopRoundTrip(spectatorPage, result);

    shouldCloseBrowser = false;
    return browser;
  } finally {
    if (shouldCloseBrowser) {
      await browser.close().catch(() => log('CLEANUP', 'Spectator browser already closed'));
    }
  }
}

function validateDiagnostics(result: TestResult, passLogEntries: LogEntry[], codec: string): void {
  log('TEST', '=== Step 4: Diagnostic Validation ===');

  const desktopLogs = passLogEntries.filter((e) => e.source === 'desktop-main' || e.source === 'desktop-renderer');
  const desktopErrors = validateLogs(desktopLogs, 'Desktop');
  result.consoleErrors.push(...desktopErrors);
  if (desktopErrors.length > 0) {
    result.errors.push(`${desktopErrors.length} suspicious desktop console log entry(s)`);
  }

  const spectatorLogs = passLogEntries.filter((e) => e.source === 'spectator');
  const spectatorErrors = validateLogs(spectatorLogs, 'Spectator');
  result.consoleErrors.push(...spectatorErrors);
  if (spectatorErrors.length > 0) {
    result.errors.push(`${spectatorErrors.length} suspicious spectator console log entry(s)`);
  }

  const gpuIssues = validateGpuReport(result.gpuReport);
  for (const issue of gpuIssues) {
    result.errors.push(`GPU: ${issue}`);
  }

  if (!result.spectatorVideoReceived && codec !== 'h265') {
    result.errors.push('Spectator did not receive video stream within timeout');
  }

  if (!result.spectatorFramesFlowing && codec !== 'h265') {
    result.errors.push('Spectator video stalled after the first frame (no continuous frame flow)');
  }
  if (!result.spectatorFrameHasContent && codec !== 'h265') {
    result.errors.push('Spectator video frames are uniformly black (capture malfunction)');
  }
  if (!result.spectatorNotifiedOfStop) {
    result.errors.push('Spectator was not informed when the presenter stopped streaming');
  }
  if (!result.presenterVideoFlowing) {
    result.errors.push('Presenter published no advancing video frames (videoFramesEncoded did not grow)');
  }
  if (result.previewFramesSent <= 0) {
    result.errors.push('Presenter emitted no preview frames (previewFramesSent stayed at 0)');
  }
}

function writeOutputArtifacts(logEntries: LogEntry[]): void {
  const consoleOutput = logEntries
    .map((e) => `[${new Date(e.timestamp).toISOString()}] [${e.source}] ${e.message}`)
    .join('\n');
  writeFileSync(DESKTOP_CONSOLE_LOG, consoleOutput);
  const webLogEntries = logEntries.filter((e) => e.source === 'spectator');
  writeFileSync(WEB_CONSOLE_LOG, webLogEntries.map((e) => e.message).join('\n'));
}

async function shutdownResources(
  browser: Browser | null,
  presenterProc: ChildProcess | null,
  procs: ServerProcs,
  config: AppConfig,
): Promise<void> {
  log('CLEANUP', 'Shutting down...');
  if (browser) {
    await browser.close().catch(() => log('CLEANUP', 'Spectator browser already closed'));
  }
  if (presenterProc) {
    writePresenterRelease();
    await waitForProcessExit(presenterProc, PRESENTER_TEARDOWN_MS);
    if (presenterProc.exitCode === null && presenterProc.signalCode === null) {
      log('CLEANUP', 'presenter script did not exit after release — killing');
      presenterProc.kill('SIGTERM');
      await waitForProcessExit(presenterProc, 5000);
    }
  }
  for (const proc of [procs.serverProc, procs.webProc, procs.livekitProc]) {
    try {
      proc?.kill('SIGTERM');
    } catch (err) {
      log('CLEANUP', `Process kill failed: ${err}`);
    }
  }

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
    spectatorCodec: null,
    spectatorDecodedFps: 0,
    spectatorFramesFlowing: false,
    spectatorFrameHasContent: false,
    spectatorNotifiedOfStop: false,
    presenterVideoFlowing: false,
    presenterVideoFramesEncoded: 0,
    presenterVideoBytesSent: 0,
    presenterTelemetryFps: 0,
    presenterBitrateBps: 0,
    captureFramesPushed: 0,
    previewFramesSent: 0,
    videoCodecReported: null,
    encoderImplementation: null,
    decoderStallDetected: false,
    codecsTested: [],
    codecResults: {},
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

  const captureMode = process.env.E2E_CAPTURE === 'portal' ? 'portal' : 'synthetic';
  const defaultCodecs = 'h264,h265,vp8,vp9,av1';
  const codecs = (process.env.E2E_CODECS ?? defaultCodecs)
    .split(',')
    .map((c) => c.trim())
    .filter(Boolean);
  result.codecsTested = codecs;
  log('TEST', `Capture mode: ${captureMode}; codecs under test: ${codecs.join(', ')}`);

  const streamSettingsPath = path.join(OUTPUT_DIR, 'e2e-userdata', 'slopcast', 'stream-settings.json');
  const writeStreamSettingsForPass = (codec: string): void => {
    if (captureMode === 'portal') return;
    const bitrateLimit = passBitrateFor(codec);
    mkdirSync(path.dirname(streamSettingsPath), { recursive: true });
    writeFileSync(
      streamSettingsPath,
      JSON.stringify(
        {
          fps: passFps,
          bitrateLimit,
          videoCodec: codec,
          resolution: passResolution,
          apiEndpoint: 'http://localhost:3001',
          autoBitrate: false,
          motionMode: 'static',
        },
        null,
        2,
      ),
    );
    log(
      'CONFIG',
      `Wrote stream settings for codec ${codec}: ${passResolution}@${passFps}, ${Math.round(bitrateLimit / 1_000_000)} Mbps`,
    );
  };

  let browser: Browser | null = null;
  let presenterProc: ChildProcess | null = null;
  let procs: ServerProcs = { serverProc: null, webProc: null, livekitProc: null };

  const releasePresenterSession = async (): Promise<void> => {
    if (presenterProc) {
      writePresenterRelease();
      await waitForProcessExit(presenterProc, PRESENTER_TEARDOWN_MS);
      if (presenterProc.exitCode === null && presenterProc.signalCode === null) {
        log('CLEANUP', 'presenter script did not exit after release — killing');
        presenterProc.kill('SIGTERM');
        await waitForProcessExit(presenterProc, 5000);
      }
      presenterProc = null;
    }
    if (browser) {
      await browser.close().catch(() => log('CLEANUP', 'Spectator browser already closed'));
      browser = null;
    }
  };

  const runCodecPass = async (codec: string): Promise<void> => {
    const errorsBefore = result.errors.length;
    const logsBefore = logEntries.length;
    result.roomCode = '';
    result.shareUrl = '';
    result.gpuReport = null;
    result.spectatorConnected = false;
    result.spectatorVideoReceived = false;
    result.spectatorVideoPlaying = false;
    result.spectatorFramesFlowing = false;
    result.spectatorFrameHasContent = false;
    result.spectatorNotifiedOfStop = false;
    result.presenterVideoFlowing = false;
    result.presenterVideoFramesEncoded = 0;
    result.presenterVideoBytesSent = 0;
    result.presenterBitrateBps = 0;
    result.presenterTelemetryFps = 0;
    result.captureFramesPushed = 0;
    result.previewFramesSent = 0;
    result.videoCodecReported = null;
    result.encoderImplementation = null;
    result.decoderStallDetected = false;
    result.spectatorDecodedFps = 0;
    result.spectatorVideoWidth = 0;
    result.spectatorVideoHeight = 0;
    result.spectatorCodec = null;
    writeStreamSettingsForPass(codec);
    log('TEST', `=== Pass: codec=${codec} (${captureMode} capture) ===`);
    let fatalError: string | null = null;
    try {
      await runPresenterPhase(config, logEntries, result, codec, captureMode, (process) => {
        presenterProc = process;
      });

      await logRoomPublications(config, result.roomCode);

      browser = await runSpectatorPhase(logEntries, result, codec, captureMode);
    } catch (err) {
      fatalError = err instanceof Error ? err.message : String(err);
      log('TEST', `Pass codec=${codec}: FATAL ${fatalError}`);
      result.errors.push(`[${codec}] ${fatalError}`);
    } finally {
      await releasePresenterSession();
    }

    if (fatalError == null) {
      validateDiagnostics(result, logEntries.slice(logsBefore), codec);
    }
    const passErrors = result.errors.slice(errorsBefore);
    result.codecResults[codec] = {
      passed: passErrors.length === 0,
      errors: passErrors,
      encoderImplementation: result.encoderImplementation ?? null,
      videoCodecReported: result.videoCodecReported ?? null,
      presenterBitrateBps: result.presenterBitrateBps,
      presenterTelemetryFps: result.presenterTelemetryFps,
      spectatorDecodedFps: result.spectatorDecodedFps,
      spectatorVideoWidth: result.spectatorVideoWidth,
      spectatorVideoHeight: result.spectatorVideoHeight,
      spectatorCodec: result.spectatorCodec,
    };
    log('TEST', `Pass codec=${codec}: ${fatalError == null && passErrors.length === 0 ? 'PASSED' : 'FAILED'}`);
  };

  try {
    procs = await ensureServers(config, logEntries);

    for (const codec of codecs) {
      await runCodecPass(codec);
    }
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    log('TEST', `FATAL: ${message}`);
    result.errors.push(message);
  } finally {
    await shutdownResources(browser, presenterProc, procs, config);
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
  log('SUMMARY', `Stop Notified:   ${result.spectatorNotifiedOfStop}`);
  log('SUMMARY', `Video Size:      ${result.spectatorVideoWidth}x${result.spectatorVideoHeight}`);
  log('SUMMARY', `Spectator FPS:   ${result.spectatorDecodedFps.toFixed(1)}`);
  log('SUMMARY', `Presenter FPS:   ${result.presenterTelemetryFps}`);
  log('SUMMARY', `Presenter Mbps:  ${(result.presenterBitrateBps / 1_000_000).toFixed(2)}`);
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
