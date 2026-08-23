export interface AppConfig {
  serverPort: number;
  webPort: number;
  apiEndpoint: string;
  websiteUrl: string;
  livekitUrl: string;
  livekitApiKey: string;
  livekitApiSecret: string;
}

export const ROOM_CODE_RE = /^[a-z]{3}-[0-9]{3}-[a-z]{3}$/;

export function normalizeLivekitUrl(url: string, pageIsHttps: boolean): string {
  if (pageIsHttps && url.startsWith('ws://')) {
    return `wss://${url.slice('ws://'.length)}`;
  }
  return url;
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

export interface AudioAppWave {
  id: number;
  columns: number[];
}

export const WAVE_EPSILON = 0.002;

export type VideoCodec = 'vp8' | 'h264' | 'vp9' | 'av1' | 'h265';

export const VIDEO_CODEC_PRIORITY: VideoCodec[] = ['h264', 'h265', 'vp8', 'vp9', 'av1'];

export type ResolutionPreset = '480p' | '720p' | '1080p' | '1440p' | '2160p';

export type MotionMode = 'auto' | 'static' | 'mixed' | 'dynamic';

export const RESOLUTION_DIMENSIONS: Record<ResolutionPreset, { width: number; height: number }> = {
  '480p': { width: 854, height: 480 },
  '720p': { width: 1280, height: 720 },
  '1080p': { width: 1920, height: 1080 },
  '1440p': { width: 2560, height: 1440 },
  '2160p': { width: 3840, height: 2160 },
};

export interface StreamSettings {
  fps: number;
  bitrateLimit: number;
  videoCodec: VideoCodec;
  resolution: ResolutionPreset;
  apiEndpoint: string;
  autoBitrate: boolean;
  motionMode: MotionMode;
}

export const DEFAULT_STREAM_SETTINGS: StreamSettings = {
  fps: 60,
  bitrateLimit: 20_000_000,
  videoCodec: 'vp8',
  resolution: '1080p',
  apiEndpoint: 'http://localhost:3001',
  autoBitrate: true,
  motionMode: 'auto',
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
  if (typeof raw !== 'object' || raw === null) {
    return { ...DEFAULT_STREAM_SETTINGS };
  }
  const o = raw as Record<string, unknown>;
  const d = DEFAULT_STREAM_SETTINGS;
  const num = (v: unknown, min: number, max: number, fallback: number): number =>
    typeof v === 'number' && Number.isFinite(v) && v >= min && v <= max ? v : fallback;
  const codec = (v: unknown): VideoCodec =>
    v === 'h264' || v === 'h265' || v === 'vp8' || v === 'vp9' || v === 'av1' ? v : d.videoCodec;
  const resolution = (v: unknown): ResolutionPreset =>
    v === '480p' || v === '720p' || v === '1080p' || v === '1440p' || v === '2160p' ? v : d.resolution;
  const motionMode = (v: unknown): MotionMode =>
    v === 'auto' || v === 'static' || v === 'mixed' || v === 'dynamic' ? v : d.motionMode;
  return {
    fps: num(o.fps, 1, 60, d.fps),
    bitrateLimit: num(o.bitrateLimit, 100_000, 200_000_000, d.bitrateLimit),
    videoCodec: codec(o.videoCodec),
    resolution: resolution(o.resolution),
    apiEndpoint: typeof o.apiEndpoint === 'string' && o.apiEndpoint.trim() !== '' ? o.apiEndpoint : d.apiEndpoint,
    autoBitrate: typeof o.autoBitrate === 'boolean' ? o.autoBitrate : d.autoBitrate,
    motionMode: motionMode(o.motionMode),
  };
}
