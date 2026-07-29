export interface AppConfig {
  serverPort: number;
  webPort: number;
  apiEndpoint: string;
  websiteUrl: string;
  livekitUrl: string;
  livekitApiKey: string;
  livekitApiSecret: string;
}

export type ClientRole = 'presenter' | 'spectator';
export type ClientOrigin = 'desktop' | 'web';

export interface Participant {
  id: string;
  role: ClientRole;
  origin: ClientOrigin;
  joinedAt: number;
}

export interface ErrorPayload {
  message: string;
  code?: string;
}

export interface AudioApp {
  id: number;
  name: string;
  processId: number;
  bundleId?: string | null;
  windowTitle?: string | null;
  clientId?: number | null;
  mediaTitle?: string | null;
}

export interface AudioAppLevel {
  id: number;
  level: number;
}

export type VideoCodec = 'vp8' | 'h264' | 'vp9' | 'av1';

export const VIDEO_CODEC_LABEL_LK: Record<VideoCodec, string> = {
  vp8: 'VP8',
  h264: 'H.264',
  vp9: 'VP9',
  av1: 'AV1',
};

export type ResolutionPreset = '1080p' | '1440p' | '2160p';

export const RESOLUTION_DIMENSIONS: Record<ResolutionPreset, { width: number; height: number }> = {
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

export const DEFAULT_STREAM_SETTINGS: StreamSettings = {
  fps: 60,
  bitrateLimit: 20_000_000,
  videoCodec: 'h264',
  resolution: '1080p',
  apiEndpoint: 'http://localhost:3001',
};

export const VIDEO_CODEC_LABEL: Record<string, string> = {
  'VIDEO/H264': 'H.264',
  'VIDEO/H265': 'H.265',
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
  const d = DEFAULT_STREAM_SETTINGS;
  if (typeof raw !== 'object' || raw === null) return d;
  const o = raw as Record<string, unknown>;
  const num = (v: unknown, min: number, max: number, fallback: number): number =>
    typeof v === 'number' && Number.isFinite(v) && v >= min && v <= max ? v : fallback;
  const codec = (v: unknown): VideoCodec =>
    v === 'h264' || v === 'vp8' || v === 'vp9' || v === 'av1' ? v : d.videoCodec;
  const resolution = (v: unknown): ResolutionPreset =>
    v === '1080p' || v === '1440p' || v === '2160p' ? v : d.resolution;
  return {
    fps: num(o.fps, 1, 240, d.fps),
    bitrateLimit: num(o.bitrateLimit, 100_000, 200_000_000, d.bitrateLimit),
    videoCodec: codec(o.videoCodec),
    resolution: resolution(o.resolution),
    apiEndpoint: typeof o.apiEndpoint === 'string' && o.apiEndpoint.trim() !== '' ? o.apiEndpoint : d.apiEndpoint,
  };
}
