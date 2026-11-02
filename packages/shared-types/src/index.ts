export interface AppConfig {
  serverPort: number;
  webPort: number;
  apiEndpoint: string;
  websiteUrl: string;
  livekitUrl: string;
  livekitApiKey: string;
  livekitApiSecret: string;
}

export const ROOM_CODE_RE = /^[a-z]{3}-[0-9]{3}-[a-z]{3}$/u;

export const normalizeLivekitUrl = (url: string, pageIsHttps: boolean): string => {
  if (pageIsHttps && url.startsWith('ws://')) {
    return `wss://${url.slice('ws://'.length)}`;
  }
  return url;
};

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
  '1080p': { height: 1080, width: 1920 },
  '1440p': { height: 1440, width: 2560 },
  '2160p': { height: 2160, width: 3840 },
  '480p': { height: 480, width: 854 },
  '720p': { height: 720, width: 1280 },
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
  apiEndpoint: 'http://localhost:3001',
  apiEndpointIsCustom: false,
  autoBitrate: true,
  bitrateLimit: 20_000_000,
  fps: 60,
  motionMode: 'auto',
  pendingApiEndpoint: null,
  resolution: '1080p',
  videoCodec: 'vp8',
};

const AUDIO_CODEC_LABELS = {
  G722: 'G.722',
  OPUS: 'Opus',
  RED: 'RED',
  TELEPHONEEVENT: 'DTMF',
} as const;

type AudioCodec = keyof typeof AUDIO_CODEC_LABELS;

const isAudioCodec = (value: string): value is AudioCodec => Object.hasOwn(AUDIO_CODEC_LABELS, value);

export const codecLabel = (mime: string | null | undefined): string | null => {
  if (mime === null || mime === undefined || mime === '') {
    return null;
  }

  const normalized = mime.toUpperCase();
  const [kind, name] = normalized.split('/');
  if (kind === 'VIDEO' && name !== undefined && name !== '') {
    const codec = VIDEO_CODECS.find((candidate) => candidate.id.toUpperCase() === name);
    if (codec) {
      return codec.label;
    }
  }
  if (kind === 'AUDIO' && name !== undefined && name !== '' && isAudioCodec(name)) {
    return AUDIO_CODEC_LABELS[name];
  }

  return mime.replace(/^(?:VIDEO|AUDIO)\//iu, '');
};

export const fmtBitrate = (bps: number | null): string => {
  if (bps === null) {
    return '\u2014';
  }
  if (bps >= 1_000_000) {
    return `${(bps / 1_000_000).toFixed(1)} Mbps`;
  }
  return `${Math.max(1, Math.round(bps / 1000))} kbps`;
};

export const fmtLoss = (pct: number | null): string => {
  if (pct === null) {
    return '—';
  }
  if (pct < 0.1) {
    return `${pct.toFixed(2)}%`;
  }
  return `${pct.toFixed(1)}%`;
};

type StreamSettingValue = string | number | boolean | null | undefined;
type StreamSettingsInputFields = Partial<Record<keyof StreamSettings, StreamSettingValue>>;
type StreamSettingsInput = StreamSettingsInputFields | StreamSettingValue | readonly StreamSettingValue[];

const isSettingsInputFields = (input: StreamSettingsInput): input is StreamSettingsInputFields =>
  input !== null && input !== undefined && !Array.isArray(input) && input.constructor === Object;

const isFiniteNumber = (value: StreamSettingValue): value is number =>
  value !== null && value !== undefined && value.constructor === Number && Number.isFinite(value);

const isString = (value: StreamSettingValue): value is string =>
  value !== null && value !== undefined && value.constructor === String;

const stringValueOr = <Value extends string>(
  input: StreamSettingValue,
  isValue: (value: string) => value is Value,
  fallback: Value,
): Value => {
  if (!isString(input) || !isValue(input)) {
    return fallback;
  }
  return input;
};

export const sanitizeStreamSettings = (input: StreamSettingsInput): StreamSettings => {
  if (!isSettingsInputFields(input)) {
    return { ...DEFAULT_STREAM_SETTINGS };
  }

  const defaults = DEFAULT_STREAM_SETTINGS;
  const numberInRange = (value: StreamSettingValue, min: number, max: number, fallback: number): number => {
    if (!isFiniteNumber(value) || value < min || value > max) {
      return fallback;
    }
    return value;
  };
  const endpoint = input.apiEndpoint;
  const pendingEndpoint = input.pendingApiEndpoint;
  const sanitizedEndpoint = isString(endpoint) && endpoint.trim() !== '' ? endpoint : defaults.apiEndpoint;
  let apiEndpointIsCustom = input.apiEndpointIsCustom === true;
  if (
    (input.apiEndpointIsCustom === null || input.apiEndpointIsCustom === undefined) &&
    sanitizedEndpoint !== defaults.apiEndpoint
  ) {
    apiEndpointIsCustom = true;
  }

  return {
    apiEndpoint: sanitizedEndpoint,
    apiEndpointIsCustom,
    autoBitrate: input.autoBitrate === true || input.autoBitrate === false ? input.autoBitrate : defaults.autoBitrate,
    bitrateLimit: numberInRange(input.bitrateLimit, 100_000, 200_000_000, defaults.bitrateLimit),
    fps: numberInRange(input.fps, 1, 60, defaults.fps),
    motionMode: stringValueOr(input.motionMode, isMotionMode, defaults.motionMode),
    pendingApiEndpoint:
      isString(pendingEndpoint) && pendingEndpoint.trim() !== '' ? pendingEndpoint : defaults.pendingApiEndpoint,
    resolution: stringValueOr(input.resolution, isResolutionPreset, defaults.resolution),
    videoCodec: stringValueOr(input.videoCodec, isVideoCodec, defaults.videoCodec),
  };
};
export * from './json.js';
