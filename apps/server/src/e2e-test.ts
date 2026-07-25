#!/usr/bin/env node
/**
 * End-to-End Test: Presenter -> Spectator Video Sharing Flow
 *
 * Validates the complete room-based screen sharing ecosystem:
 *   1. Parse slopcast.config.json for ports and endpoints
 *   2. Kill conflicting processes, spawn server + web dev servers
 *   3. Launch Electron presenter: create room, share screen
 *   4. Launch Chromium spectator: join room, verify video stream
 *   5. Diagnostic validation: console logs, GPU report, stream health
 *   6. Graceful cleanup with retry-on-failure logic
 *
 * Prerequisites:
 *   Playwright + Chromium: pnpm add -D -w playwright && npx playwright install chromium
 *   Desktop app built:      pnpm build:desktop
 *
 * Usage:
 *   pnpm tsx apps/server/src/e2e-test.ts
 */

import { type ChildProcess, execSync, spawn } from 'node:child_process';
import { existsSync, writeFileSync } from 'node:fs';
import http from 'node:http';
import path from 'node:path';

import { loadConfig } from '@slopcast/shared-types/config';

import type { Page } from 'playwright';

// ── Configuration Types ────────────────────────────────────────────────

interface GpuFeatureStatus {
  name: string;
  status: string;
}

interface GpuInfo {
  gpuDevice: Array<Record<string, unknown>>;
  featureStatus: GpuFeatureStatus[];
  problems: string[];
  auxAttributes?: Record<string, unknown>;
}

interface LogEntry {
  source: 'server' | 'web' | 'desktop-main' | 'desktop-renderer' | 'spectator';
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
  durationMs: number;
  retries: number;
  errors: string[];
}

// ── Constants ──────────────────────────────────────────────────────────

const REPO_ROOT = path.resolve(__dirname, '..', '..', '..');
const OUTPUT_DIR = path.join(REPO_ROOT, 'test-output');
const DESKTOP_CONSOLE_LOG = path.join(OUTPUT_DIR, 'desktop-console.log');
const WEB_CONSOLE_LOG = path.join(OUTPUT_DIR, 'web-console.log');
const GPU_REPORT_PATH = path.join(OUTPUT_DIR, 'desktop-gpu-report.json');
const RESULT_PATH = path.join(OUTPUT_DIR, 'e2e-result.json');

const HEALTH_POLL_MS = 500;
const STARTUP_TIMEOUT_MS = 30_000;
const ROOM_CREATE_TIMEOUT_MS = 15_000;
const SPECTATOR_CONNECT_TIMEOUT_MS = 20_000;
const STREAM_TIMEOUT_MS = 20_000;
const TEST_TOTAL_TIMEOUT_MS = 120_000;

const FATAL_PATTERNS = [
  /(?:FATAL|fatal)\s*error/i,
  /uncaught\s*(?:exception|error)/i,
  /cannot read propert(?:y|ies) of (?:undefined|null)/i,
  /process\s*\.\s*exit/i,
  /segmentation\s*fault/i,
  /signal\s*:\s*SIG(?:SEGV|ABRT|ILL|FPE|BUS)/i,
  /ERR_MODULE_NOT_FOUND/,
  /Failed to load resource/,
  /WebSocket is closed before the connection is established/,
  /iceConnectionState.*failed/i,
  /GPU process.*crash/i,
];

// ── Utility Helpers ────────────────────────────────────────────────────

function log(prefix: string, msg: string): void {
  const ts = new Date().toISOString().slice(11, 23);
  process.stdout.write(`[${ts}] [${prefix}] ${msg}\n`);
}

function killPort(port: number): void {
  const platform = process.platform;
  try {
    if (platform === 'linux' || platform === 'darwin') {
      execSync(`fuser -k ${port}/tcp 2>/dev/null || true`, { stdio: 'pipe' });
    } else if (platform === 'win32') {
      execSync(`netstat -ano | findstr :${port}`, { stdio: 'pipe' });
    }
    log('CLEANUP', `Killed processes on port ${port}`);
  } catch {
    // Port was free — no-op.
  }
}

function httpGet(url: string): Promise<number> {
  return new Promise((resolve, reject) => {
    const req = http
      .get(url, (res) => {
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
      // Not ready yet.
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

// ── Spotify Detection ──────────────────────────────────────────────────

function findSpotifyProcess(): boolean {
  const platform = process.platform;
  try {
    if (platform === 'linux') {
      const out = execSync('pgrep -x spotify 2>/dev/null || true', { encoding: 'utf-8' }).trim();
      return out.length > 0;
    }
    if (platform === 'darwin') {
      const out = execSync('pgrep -x Spotify 2>/dev/null || true', { encoding: 'utf-8' }).trim();
      return out.length > 0;
    }
    if (platform === 'win32') {
      const out = execSync('tasklist /FI "IMAGENAME eq Spotify.exe" 2>nul', { encoding: 'utf-8' });
      return out.includes('Spotify.exe');
    }
  } catch {
    // pgrep/tasklist not available.
  }
  return false;
}

function launchSpotify(): boolean {
  const platform = process.platform;
  try {
    if (platform === 'linux') {
      execSync('nohup spotify >/dev/null 2>&1 &', { stdio: 'ignore', timeout: 3000 });
      return true;
    }
    if (platform === 'darwin') {
      execSync('open -a Spotify', { stdio: 'ignore', timeout: 3000 });
      return true;
    }
    if (platform === 'win32') {
      execSync('start "" "spotify:"', { stdio: 'ignore', timeout: 3000 });
      return true;
    }
  } catch {
    // Spotify not installed or failed to launch.
  }
  return false;
}

// ── Log Validation ─────────────────────────────────────────────────────

function parseAndValidateLogs(entries: LogEntry[], label: string): { errors: LogEntry[]; fatals: LogEntry[] } {
  const errors: LogEntry[] = [];
  const fatals: LogEntry[] = [];

  for (const entry of entries) {
    for (const pattern of FATAL_PATTERNS) {
      if (pattern.test(entry.message)) {
        if (entry.message.toLowerCase().includes('error') || entry.message.toLowerCase().includes('fatal')) {
          fatals.push(entry);
        } else {
          errors.push(entry);
        }
        break;
      }
    }
  }

  if (fatals.length > 0) {
    log('DIAGNOSTIC', `${label}: ${fatals.length} fatal error(s) detected`);
    for (const f of fatals) {
      log('DIAGNOSTIC', `  [${f.source}] ${f.message.slice(0, 200)}`);
    }
  }

  return { errors, fatals };
}

function validateGpuReport(report: GpuInfo | null): string[] {
  const issues: string[] = [];

  if (!report) {
    issues.push('GPU report is null — GPU info could not be retrieved');
    return issues;
  }

  const featureStatus = report.featureStatus ?? [];
  const problems: string[] = report.problems ?? [];

  const hasSoftwareRenderer =
    featureStatus.some(
      (f) => f.name === 'gpu_compositing' && (f.status.includes('Software') || f.status.includes('disabled')),
    ) || problems.some((p) => p.toLowerCase().includes('software'));

  if (hasSoftwareRenderer) {
    issues.push('GPU acceleration appears disabled or software-rendered');
  }

  const criticalDisabled = featureStatus.filter(
    (f) =>
      (f.name === 'webgl' || f.name === 'webgl2' || f.name === 'video_encode') &&
      !f.status.toLowerCase().includes('enabled') &&
      !f.status.toLowerCase().includes('accelerated') &&
      !f.status.toLowerCase().includes('hardware'),
  );

  if (criticalDisabled.length > 0) {
    for (const f of criticalDisabled) {
      issues.push(`GPU feature "${f.name}" is not enabled: ${f.status}`);
    }
  }

  if (problems.length > 0) {
    for (const p of problems) {
      issues.push(`GPU problem: ${p.slice(0, 200)}`);
    }
  }

  return issues;
}

// ── Main Test Orchestrator ─────────────────────────────────────────────

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
    durationMs: 0,
    retries: 0,
    errors: [],
  };

  const logEntries: LogEntry[] = [];

  // ── Step 1: Dynamic Configuration & Environment Setup ────────────────
  log('TEST', '=== Step 1: Configuration & Environment Setup ===');

  const config = loadConfig();
  log('CONFIG', `serverPort=${config.serverPort} webPort=${config.webPort}`);
  log('CONFIG', `apiEndpoint=${config.apiEndpoint} websiteUrl=${config.websiteUrl}`);

  // Kill conflicting processes on configured ports.
  killPort(config.serverPort);
  killPort(config.webPort);
  await new Promise((r) => setTimeout(r, 500));

  // Spawn API server.
  log('SPAWN', 'Starting API server...');
  const serverProc = spawnLogging('pnpm', ['--filter', 'server', 'dev'], 'server', logEntries);
  await pollHealth(`${config.apiEndpoint}/health`, STARTUP_TIMEOUT_MS, 'API server');

  // Spawn Web server.
  log('SPAWN', 'Starting Web dev server...');
  const webProc = spawnLogging('pnpm', ['--filter', 'web', 'dev'], 'web', logEntries);
  await pollHealth(config.websiteUrl, STARTUP_TIMEOUT_MS, 'Web server');

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

  // ── Step 2: Presenter Automation (Electron via Playwright) ───────────
  log('TEST', '=== Step 2: Presenter Automation (Electron) ===');

  // Dynamic import to avoid type issues at top level before playwright is installed.
  // eslint-disable-next-line @typescript-eslint/no-require-imports

  const { chromium, _electron: electron } = await import('playwright');

  const desktopDir = path.join(REPO_ROOT, 'apps', 'desktop');
  const electronBin = path.join(desktopDir, 'node_modules', 'electron', 'dist', 'electron');

  if (!existsSync(electronBin)) {
    throw new Error(`Electron binary not found at ${electronBin}. Run "pnpm install" and "pnpm build:desktop" first.`);
  }

  log('ELECTRON', `Launching Electron from ${electronBin}`);
  log('ELECTRON', `App directory: ${desktopDir}`);

  const electronApp = await electron.launch({
    executablePath: electronBin,
    args: [desktopDir],
    env: {
      ...process.env,
      NODE_ENV: 'test',
    },
    timeout: 30_000,
  });

  // Capture main process console via pipe.
  electronApp.process().stdout?.on('data', (data: Buffer) => {
    for (const line of data.toString().split('\n').filter(Boolean)) {
      logEntries.push({ source: 'desktop-main', message: line, timestamp: Date.now() });
    }
  });
  electronApp.process().stderr?.on('data', (data: Buffer) => {
    for (const line of data.toString().split('\n').filter(Boolean)) {
      logEntries.push({ source: 'desktop-main', message: line, timestamp: Date.now() });
    }
  });

  // Wait for first window and capture renderer console.
  const page: Page = await electronApp.firstWindow();

  page.on('console', (msg) => {
    logEntries.push({
      source: 'desktop-renderer',
      message: `[${msg.type()}] ${msg.text()}`,
      timestamp: Date.now(),
    });
  });

  page.on('pageerror', (err) => {
    logEntries.push({
      source: 'desktop-renderer',
      message: `UNCAUGHT: ${err.message}`,
      timestamp: Date.now(),
    });
  });

  await page.waitForLoadState('domcontentloaded');
  log('ELECTRON', 'Desktop window loaded');

  // Click "Create Live Room" button.
  const createRoomBtn = page.locator('button', { hasText: 'Create Live Room' });
  await createRoomBtn.waitFor({ state: 'visible', timeout: ROOM_CREATE_TIMEOUT_MS });
  log('ELECTRON', '"Create Live Room" button visible');

  await createRoomBtn.click();
  log('ELECTRON', 'Clicked "Create Live Room"');

  // Wait for room code to appear (replaces the Create button).
  const roomCodeSpan = page.locator('span.font-mono');
  await roomCodeSpan.waitFor({ state: 'visible', timeout: ROOM_CREATE_TIMEOUT_MS });
  const roomCode = (await roomCodeSpan.textContent())?.trim() ?? '';
  result.roomCode = roomCode;
  result.shareUrl = `${config.websiteUrl}/room/${roomCode}`;
  log('ELECTRON', `Room created: code=${roomCode} url=${result.shareUrl}`);

  if (!roomCode) {
    throw new Error('Failed to extract room code from Electron UI');
  }

  // Attempt to select and share a screen source.
  // On X11: select a source thumbnail; on Wayland: portal dialog is native.
  const isWaylandBtn = page.locator('text=/Wayland|X11/');
  const platformText = (await isWaylandBtn.textContent()) ?? '';
  const isX11 = platformText.includes('X11');
  const isWaylandPlatform = platformText.includes('Wayland');

  log('ELECTRON', `Platform detected: ${platformText.trim()}`);

  if (isX11) {
    // Select the first available screen source thumbnail.
    const sourceBtns = page.locator('button:has(img)');
    const sourceCount = await sourceBtns.count();
    if (sourceCount > 0) {
      await sourceBtns.first().click();
      log('ELECTRON', `Selected screen source (${sourceCount} available)`);
      await new Promise((r) => setTimeout(r, 500));
    } else {
      log('ELECTRON', 'No screen source thumbnails found');
    }
  }

  // Click "Start Screenshare".
  const startBtn = page.locator('button', { hasText: 'Start Screenshare' });
  try {
    await startBtn.waitFor({ state: 'visible', timeout: 5000 });
    const isDisabled = await startBtn.isDisabled();
    if (!isDisabled) {
      await startBtn.click();
      log('ELECTRON', 'Clicked "Start Screenshare"');

      if (isWaylandPlatform) {
        log('ELECTRON', 'Wayland portal dialog may need manual interaction (xdg-desktop-portal)');
      }
    } else {
      log('ELECTRON', '"Start Screenshare" button is disabled — may need source selection');
    }
  } catch {
    log('ELECTRON', '"Start Screenshare" button not found or not available');
  }

  // Give stream time to start.  On X11 the thumbnail picker is instant;
  // on Wayland the xdg-desktop-portal dialog requires manual interaction.
  // Poll the LIVE badge for up to STREAM_TIMEOUT_MS.
  {
    const deadline = Date.now() + STREAM_TIMEOUT_MS;
    let isLive = false;
    log('ELECTRON', `Waiting for streaming to start (timeout ${STREAM_TIMEOUT_MS}ms)...`);
    while (Date.now() < deadline) {
      const liveBadge = page.locator('text=LIVE');
      if ((await liveBadge.count()) > 0) {
        isLive = true;
        break;
      }
      await new Promise((r) => setTimeout(r, 1000));
    }
    log('ELECTRON', `Streaming live: ${isLive}`);
  }

  // ── GPU Diagnostic Data ──────────────────────────────────────────
  log('ELECTRON', 'Retrieving GPU diagnostic data...');
  let gpuInfo: GpuInfo | null = null;
  try {
    gpuInfo = (await electronApp.evaluate(async ({ app }) => {
      try {
        return await app.getGPUInfo('complete');
      } catch {
        return null;
      }
    })) as GpuInfo | null;

    if (gpuInfo) {
      writeFileSync(GPU_REPORT_PATH, JSON.stringify(gpuInfo, null, 2));
      log('ELECTRON', `GPU report written to ${GPU_REPORT_PATH}`);
      result.gpuReport = gpuInfo;
    }
  } catch (err) {
    log('ELECTRON', `Failed to retrieve GPU info: ${err}`);
  }

  // ── Step 3: Spectator Automation (Chromium via Playwright) ───────────
  log('TEST', '=== Step 3: Spectator Automation (Chromium) ===');

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
  await spectatorPage.goto(result.shareUrl, { waitUntil: 'networkidle', timeout: SPECTATOR_CONNECT_TIMEOUT_MS });

  // Wait for connection status badge.
  try {
    await spectatorPage.waitForFunction(
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
  } catch {
    log('SPECTATOR', 'Connection status badge never appeared');
    result.errors.push('Spectator never reached connecting/live state');
  }

  // Wait for video element to appear and start playing.
  try {
    await spectatorPage.waitForSelector('video', { state: 'attached', timeout: STREAM_TIMEOUT_MS });

    // Poll for video frames — the element may appear before frames decode.
    await spectatorPage.waitForFunction(
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

    const videoState = await spectatorPage.evaluate(() => {
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
  } catch {
    log('SPECTATOR', 'Video element never appeared');
    result.errors.push('Spectator video element never appeared');
  }

  // Additional stability wait to let stream settle.
  await new Promise((r) => setTimeout(r, 3000));

  // ── Step 4: Diagnostic Validation ────────────────────────────────────
  log('TEST', '=== Step 4: Diagnostic Validation ===');

  // Write console logs.
  const consoleOutput = logEntries
    .map((e) => `[${new Date(e.timestamp).toISOString()}] [${e.source}] ${e.message}`)
    .join('\n');
  writeFileSync(DESKTOP_CONSOLE_LOG, consoleOutput);
  log('DIAGNOSTIC', `Desktop console log written (${logEntries.length} entries)`);

  // Write web spectator logs separately.
  const webLogEntries = logEntries.filter((e) => e.source === 'spectator');
  writeFileSync(WEB_CONSOLE_LOG, webLogEntries.map((e) => e.message).join('\n'));
  log('DIAGNOSTIC', `Web console log written (${webLogEntries.length} entries)`);

  // Parse desktop logs for fatal errors.
  const desktopLogs = logEntries.filter((e) => e.source === 'desktop-main' || e.source === 'desktop-renderer');
  const desktopValidation = parseAndValidateLogs(desktopLogs, 'Desktop');
  result.consoleErrors = [...desktopValidation.fatals, ...desktopValidation.errors];

  if (desktopValidation.fatals.length > 0) {
    result.errors.push(`${desktopValidation.fatals.length} fatal desktop console error(s)`);
  }

  // Parse spectator logs.
  const spectatorLogs = logEntries.filter((e) => e.source === 'spectator');
  const spectatorValidation = parseAndValidateLogs(spectatorLogs, 'Spectator');
  result.consoleErrors.push(...spectatorValidation.fatals, ...spectatorValidation.errors);

  if (spectatorValidation.fatals.length > 0) {
    result.errors.push(`${spectatorValidation.fatals.length} fatal spectator console error(s)`);
  }

  // Validate GPU report.
  const gpuIssues = validateGpuReport(gpuInfo);
  if (gpuIssues.length > 0) {
    for (const issue of gpuIssues) {
      result.errors.push(`GPU: ${issue}`);
    }
  }

  // Validate spectator stream receipt.
  if (!result.spectatorVideoReceived) {
    result.errors.push('Spectator did not receive video stream within timeout');
  }

  // ── Cleanup ───────────────────────────────────────────────────────
  log('CLEANUP', 'Shutting down...');

  // Close spectator browser.
  try {
    await context.close();
    await browser.close();
    log('CLEANUP', 'Spectator browser closed');
  } catch {
    // Already closed.
  }

  // Close Electron app.
  try {
    await electronApp.close();
    log('CLEANUP', 'Electron app closed');
  } catch {
    // Already closed.
  }

  // Kill spawned processes.
  for (const proc of [serverProc, webProc]) {
    try {
      proc.kill('SIGTERM');
    } catch {
      // Already dead.
    }
  }

  // Ensure ports are freed.
  killPort(config.serverPort);
  killPort(config.webPort);

  // ── Final Assessment ──────────────────────────────────────────────
  result.passed = result.errors.length === 0;
  result.durationMs = Date.now() - startTime;

  writeFileSync(RESULT_PATH, JSON.stringify(result, null, 2));
  log('TEST', `Result written to ${RESULT_PATH}`);

  return result;
}

// ── Entry Point with Retry ─────────────────────────────────────────────

async function main(): Promise<void> {
  const maxRetries = 2;
  let lastResult: TestResult | null = null;

  for (let attempt = 1; attempt <= maxRetries; attempt++) {
    log('TEST', `========================================`);
    log('TEST', `Attempt ${attempt} of ${maxRetries}`);
    log('TEST', `========================================`);

    try {
      const result = await Promise.race([
        runTest(),
        new Promise<never>((_, reject) => setTimeout(() => reject(new Error('Test timed out')), TEST_TOTAL_TIMEOUT_MS)),
      ]);

      result.retries = attempt - 1;
      lastResult = result;

      if (result.passed) {
        log('TEST', 'RESULT: PASSED');
        printSummary(result);
        process.exit(0);
      }

      log('TEST', `RESULT: FAILED (${result.errors.length} error(s))`);
      printSummary(result);

      if (attempt < maxRetries) {
        log('TEST', `Retrying in 3 seconds...`);
        await new Promise((r) => setTimeout(r, 3000));
      }
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      log('TEST', `FATAL: ${message}`);

      if (attempt < maxRetries) {
        log('TEST', `Retrying in 3 seconds...`);
        await new Promise((r) => setTimeout(r, 3000));
      }
    }
  }

  // All retries exhausted.
  if (lastResult) {
    log('TEST', 'All retries exhausted. Final result: FAILED');
    log('TEST', `Root cause summary: ${lastResult.errors.join('; ') || 'Unknown failure'}`);
  } else {
    log('TEST', 'All retries exhausted. No result produced.');
  }
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
  log('SUMMARY', `GPU:             ${result.gpuReport ? 'Retrieved' : 'Missing'}`);
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
