import type { StreamSettings } from '@slopcast/shared-types';

export const STATS_POLL_MS = 1000;
export const STATS_HISTORY_MAX = 48;
export const AUDIO_APPS_POLL_MS = 10_000;
export const SETTINGS_SAVE_DEBOUNCE_MS = 800;

export const streamSettingsEqual = (a: StreamSettings, b: StreamSettings): boolean =>
  a.fps === b.fps &&
  a.bitrateLimit === b.bitrateLimit &&
  a.videoCodec === b.videoCodec &&
  a.resolution === b.resolution &&
  a.apiEndpoint === b.apiEndpoint;
