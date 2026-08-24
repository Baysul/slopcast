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

export const VIDEO_CODECS = [
  { id: 'h264', label: 'H.264' },
  { id: 'h265', label: 'H.265' },
  { id: 'vp8', label: 'VP8' },
  { id: 'vp9', label: 'VP9' },
  { id: 'av1', label: 'AV1' },
] as const;
export type VideoCodec = (typeof VIDEO_CODECS)[number]['id'];

export const RESOLUTION_DIMENSIONS = {
  '480p': { width: 854, height: 480 },
  '720p': { width: 1280, height: 720 },
  '1080p': { width: 1920, height: 1080 },
  '1440p': { width: 2560, height: 1440 },
  '2160p': { width: 3840, height: 2160 },
} as const;
export type ResolutionPreset = keyof typeof RESOLUTION_DIMENSIONS;

export const MOTION_MODES = ['auto', 'static', 'mixed', 'dynamic'] as const;
export type MotionMode = (typeof MOTION_MODES)[number];

export const isVideoCodec = (value: string): value is VideoCodec => VIDEO_CODECS.some((codec) => codec.id === value);

export const isResolutionPreset = (value: string): value is ResolutionPreset =>
  Object.hasOwn(RESOLUTION_DIMENSIONS, value);

export const isMotionMode = (value: string): value is MotionMode => MOTION_MODES.some((mode) => mode === value);

export interface StreamSettings {
  fps: number;
  bitrateLimit: number;
  videoCodec: VideoCodec;
  resolution: ResolutionPreset;
  apiEndpoint: string;
  apiEndpointIsCustom: boolean;
  pendingApiEndpoint: string | null;
  autoBitrate: boolean;
  motionMode: MotionMode;
}

export const DEFAULT_STREAM_SETTINGS: StreamSettings = {
  fps: 60,
  bitrateLimit: 20_000_000,
  videoCodec: 'vp8',
  resolution: '1080p',
  apiEndpoint: 'http://localhost:3001',
  apiEndpointIsCustom: false,
  pendingApiEndpoint: null,
  autoBitrate: true,
  motionMode: 'auto',
};

const AUDIO_CODEC_LABELS = {
  OPUS: 'Opus',
  RED: 'RED',
  G722: 'G.722',
  TELEPHONEEVENT: 'DTMF',
} as const;

type AudioCodec = keyof typeof AUDIO_CODEC_LABELS;

const isAudioCodec = (value: string): value is AudioCodec => Object.hasOwn(AUDIO_CODEC_LABELS, value);

export const codecLabel = (mime: string | null | undefined): string | null => {
  if (!mime) return null;

  const normalized = mime.toUpperCase();
  const [kind, name] = normalized.split('/');
  if (kind === 'VIDEO' && name) {
    const codec = VIDEO_CODECS.find((candidate) => candidate.id.toUpperCase() === name);
    if (codec) return codec.label;
  }
  if (kind === 'AUDIO' && name && isAudioCodec(name)) return AUDIO_CODEC_LABELS[name];

  return mime.replace(/^(VIDEO|AUDIO)\//i, '');
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

type StreamSettingValue = string | number | boolean | null | undefined;
type StreamSettingsInputFields = Partial<Record<keyof StreamSettings, StreamSettingValue>>;
type StreamSettingsInput = StreamSettingsInputFields | StreamSettingValue | readonly StreamSettingValue[];

const isSettingsInputFields = (input: StreamSettingsInput): input is StreamSettingsInputFields =>
  input != null && !Array.isArray(input) && input.constructor === Object;

const isFiniteNumber = (value: StreamSettingValue): value is number =>
  value != null && value.constructor === Number && Number.isFinite(Number(value));

const isString = (value: StreamSettingValue): value is string => value != null && value.constructor === String;

const stringValueOr = <Value extends string>(
  input: StreamSettingValue,
  isValue: (value: string) => value is Value,
  fallback: Value,
): Value => {
  if (!isString(input) || !isValue(input)) return fallback;
  return input;
};

export function sanitizeStreamSettings(input: StreamSettingsInput): StreamSettings {
  if (!isSettingsInputFields(input)) {
    return { ...DEFAULT_STREAM_SETTINGS };
  }

  const defaults = DEFAULT_STREAM_SETTINGS;
  const numberInRange = (value: StreamSettingValue, min: number, max: number, fallback: number): number => {
    if (!isFiniteNumber(value) || value < min || value > max) return fallback;
    return value;
  };
  const endpoint = input.apiEndpoint;
  const pendingEndpoint = input.pendingApiEndpoint;
  const sanitizedEndpoint = isString(endpoint) && endpoint.trim() !== '' ? endpoint : defaults.apiEndpoint;
  let apiEndpointIsCustom = input.apiEndpointIsCustom === true;
  if (input.apiEndpointIsCustom == null && sanitizedEndpoint !== defaults.apiEndpoint) apiEndpointIsCustom = true;

  return {
    fps: numberInRange(input.fps, 1, 60, defaults.fps),
    bitrateLimit: numberInRange(input.bitrateLimit, 100_000, 200_000_000, defaults.bitrateLimit),
    videoCodec: stringValueOr(input.videoCodec, isVideoCodec, defaults.videoCodec),
    resolution: stringValueOr(input.resolution, isResolutionPreset, defaults.resolution),
    apiEndpoint: sanitizedEndpoint,
    apiEndpointIsCustom,
    pendingApiEndpoint:
      isString(pendingEndpoint) && pendingEndpoint.trim() !== '' ? pendingEndpoint : defaults.pendingApiEndpoint,
    autoBitrate: input.autoBitrate === true || input.autoBitrate === false ? input.autoBitrate : defaults.autoBitrate,
    motionMode: stringValueOr(input.motionMode, isMotionMode, defaults.motionMode),
  };
}
