import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { type StreamSettings, sanitizeStreamSettings } from '@slopcast/shared-types';
import { app } from 'electron';

// ── Stream Settings Persistence ─────────────────────────────────────────
// Stored as JSON in Electron's per-platform user-data directory
// (%APPDATA%/<app-name> on Windows, ~/.config/<app-name> on Linux).
const STREAM_SETTINGS_FILE = 'stream-settings.json';
let streamSettingsCache: StreamSettings | null = null;

export const userDataFile = (filename: string): string => path.join(app.getPath('userData'), filename);

const streamSettingsPath = (): string => userDataFile(STREAM_SETTINGS_FILE);

export function loadStreamSettings(): StreamSettings {
  if (streamSettingsCache) return streamSettingsCache;
  const file = streamSettingsPath();
  let parsed: unknown = null;
  if (existsSync(file)) {
    try {
      parsed = JSON.parse(readFileSync(file, 'utf-8'));
    } catch (err) {
      console.error(`Failed to parse ${STREAM_SETTINGS_FILE}, using defaults:`, err);
    }
  }
  streamSettingsCache = sanitizeStreamSettings(parsed);
  return streamSettingsCache;
}

export function saveStreamSettings(raw: unknown): boolean {
  const settings = sanitizeStreamSettings(raw);
  try {
    writeFileSync(streamSettingsPath(), `${JSON.stringify(settings, null, 2)}\n`, 'utf-8');
    streamSettingsCache = settings;
    return true;
  } catch (err) {
    console.error(`Failed to write ${STREAM_SETTINGS_FILE}:`, err);
    return false;
  }
}

// ── Onboarding State Persistence ────────────────────────────────────────
const ONBOARDING_FILE = 'onboarding.json';

export function isOnboardingCompleted(): boolean {
  const file = userDataFile(ONBOARDING_FILE);
  if (!existsSync(file)) return false;
  try {
    const data = JSON.parse(readFileSync(file, 'utf-8'));
    return data?.completed === true;
  } catch {
    return false;
  }
}

export function setOnboardingCompleted(): boolean {
  try {
    writeFileSync(userDataFile(ONBOARDING_FILE), JSON.stringify({ completed: true }), 'utf-8');
    return true;
  } catch (err) {
    console.error(`Failed to write ${ONBOARDING_FILE}:`, err);
    return false;
  }
}
