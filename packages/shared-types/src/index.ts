export interface AppConfig {
  serverPort: number;
  webPort: number;
  apiEndpoint: string;
  websiteUrl: string;
  livekitUrl: string;
  livekitApiKey: string;
  livekitApiSecret: string;
}

/// The canonical room-code format, validated identically by the server and the
/// web join form: `abc-123-xyz`.
export const ROOM_CODE_RE = /^[a-z]{3}-[0-9]{3}-[a-z]{3}$/;

export interface AudioApp {
  id: number;
  name: string;
  processId: number;
  bundleId?: string | null;
  windowTitle?: string | null;
  clientId?: number | null;
  mediaTitle?: string | null;
}

export interface AudioAppWave {
  id: number;
  /** 96 interleaved (min, max) amplitude pairs of the last ~85 ms of audio. */
  columns: number[];
}

/// Waveform columns below this amplitude delta are not worth re-rendering;
/// shared by the main-process push filter and the renderer meter store so the
/// two epsilons can never drift apart.
export const WAVE_EPSILON = 0.002;

export type VideoCodec = 'vp8' | 'h264' | 'vp9' | 'av1';

export const VIDEO_CODEC_PRIORITY: VideoCodec[] = ['av1', 'vp9', 'h264', 'vp8'];

export type ResolutionPreset = '480p' | '720p' | '1080p' | '1440p' | '2160p';

export const RESOLUTION_DIMENSIONS: Record<ResolutionPreset, { width: number; height: number }> = {
  '480p': { width: 854, height: 480 },
  '720p': { width: 1280, height: 720 },
  '1080p': { width: 1920, height: 1080 },
  '1440p': { width: 2560, height: 1440 },
  '2160p': { width: 3840, height: 2160 },
};

// User-configurable encoder parameters, persisted by the desktop app to a
// JSON file in the per-platform user-data directory.
export interface StreamSettings {
  fps: number;
  bitrateLimit: number;
  videoCodec: VideoCodec;
  resolution: ResolutionPreset;
  apiEndpoint: string;
}

// TS↔Rust sync rule: `DEFAULT_STREAM_SETTINGS` and `sanitizeStreamSettings`
// are mirrored field-for-field by `default_stream_settings` and
// `sanitize_stream_settings` in apps/desktop/src-tauri/src/settings.rs
// (same defaults, clamps and whitelists). Update both files together; the
// Rust `defaults_match_ts_table` conformance test enforces these values.
export const DEFAULT_STREAM_SETTINGS: StreamSettings = {
  fps: 60,
  bitrateLimit: 20_000_000,
  videoCodec: 'vp8',
  resolution: '1080p',
  apiEndpoint: 'http://localhost:3001',
};

export const VIDEO_CODEC_LABEL: Record<string, string> = {
  'VIDEO/H264': 'H.264',
  'VIDEO/VP8': 'VP8',
  'VIDEO/VP9': 'VP9',
  'VIDEO/AV1': 'AV1',
  'AUDIO/OPUS': 'Opus',
  'AUDIO/RED': 'RED',
  'AUDIO/G722': 'G.722',
  'AUDIO/TELEPHONEEVENT': 'DTMF',
};

export const codecLabel = (mime: string | null | undefined): string | null => {
  if (!mime) return null;
  return VIDEO_CODEC_LABEL[mime.toUpperCase()] ?? mime.replace(/^(VIDEO|AUDIO)\//i, '');
};

export const fmtBitrate = (bps: number | null): string => {
  if (bps == null) return '\u2014';
  if (bps >= 1_000_000) return `${(bps / 1_000_000).toFixed(1)} Mbps`;
  return `${Math.max(1, Math.round(bps / 1000))} kbps`;
};

export const fmtLoss = (pct: number | null): string => {
  if (pct == null) return '—';
  if (pct < 0.1) return `${pct.toFixed(2)}%`;
  return `${pct.toFixed(1)}%`;
};

export function sanitizeStreamSettings(raw: unknown): StreamSettings {
  if (typeof raw !== 'object' || raw === null) {
    // Defensive copy: callers could otherwise mutate the shared default.
    return { ...DEFAULT_STREAM_SETTINGS };
  }
  const o = raw as Record<string, unknown>;
  const d = DEFAULT_STREAM_SETTINGS;
  const num = (v: unknown, min: number, max: number, fallback: number): number =>
    typeof v === 'number' && Number.isFinite(v) && v >= min && v <= max ? v : fallback;
  const codec = (v: unknown): VideoCodec =>
    v === 'h264' || v === 'vp8' || v === 'vp9' || v === 'av1' ? v : d.videoCodec;
  const resolution = (v: unknown): ResolutionPreset =>
    v === '480p' || v === '720p' || v === '1080p' || v === '1440p' || v === '2160p' ? v : d.resolution;
  return {
    fps: num(o.fps, 1, 240, d.fps),
    bitrateLimit: num(o.bitrateLimit, 100_000, 200_000_000, d.bitrateLimit),
    videoCodec: codec(o.videoCodec),
    resolution: resolution(o.resolution),
    apiEndpoint: typeof o.apiEndpoint === 'string' && o.apiEndpoint.trim() !== '' ? o.apiEndpoint : d.apiEndpoint,
  };
}
