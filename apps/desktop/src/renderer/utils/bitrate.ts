import type { ResolutionPreset, VideoCodec } from '@slopcast/shared-types';

export type MotionMode = 'auto' | 'static' | 'mixed' | 'dynamic';
export type MotionTier = Exclude<MotionMode, 'auto'>;

const AV1_SOFTWARE_CEILING_BPS: Record<ResolutionPreset, number> = {
  '480p': 2_000_000,
  '720p': 4_000_000,
  '1080p': 8_000_000,
  '1440p': 12_000_000,
  '2160p': 20_000_000,
};

const CODEC_SCALE: Record<VideoCodec, number> = {
  av1: 1.0,
  vp9: 1.4,
  h265: 1.4,
  vp8: 1.7,
  h264: 1.5,
};

const MOTION_FACTOR: Record<MotionTier, number> = {
  static: 1.0,
  mixed: 1.25,
  dynamic: 1.5,
};

const MANUAL_OPTIONS: Record<VideoCodec, number[]> = {
  h264: [1_000_000, 2_000_000, 4_000_000, 6_000_000, 10_000_000, 20_000_000, 30_000_000, 50_000_000],
  h265: [1_000_000, 2_000_000, 4_000_000, 6_000_000, 10_000_000, 20_000_000, 30_000_000, 50_000_000],
  vp8: [1_000_000, 2_000_000, 4_000_000, 6_000_000, 10_000_000, 20_000_000, 30_000_000, 50_000_000],
  vp9: [1_000_000, 2_000_000, 4_000_000, 6_000_000, 10_000_000, 20_000_000, 30_000_000, 50_000_000],
  av1: [1_000_000, 2_000_000, 4_000_000, 6_000_000, 8_000_000, 12_000_000, 16_000_000, 20_000_000],
};

export interface BitrateInput {
  codec: VideoCodec;
  resolution: ResolutionPreset;
  fps: number;
  motionTier: MotionTier;
}

const fpsScale = (fps: number): number => {
  const clamped = Math.max(1, Math.min(60, fps));
  return 0.6 + 0.4 * (clamped / 60);
};

export const recommendBitrateCap = (input: BitrateInput): number => {
  const base = AV1_SOFTWARE_CEILING_BPS[input.resolution];
  const scale = CODEC_SCALE[input.codec] * MOTION_FACTOR[input.motionTier];
  const raw = base * fpsScale(input.fps) * scale;

  return Math.max(1_000_000, Math.round(raw / 500_000) * 500_000);
};

export const recommendedBitrateRange = (input: BitrateInput): readonly [number, number] => {
  const maximum = recommendBitrateCap(input);
  const minimum = Math.max(1_000_000, Math.round((maximum * 2) / 3 / 500_000) * 500_000);

  return [minimum, maximum];
};

export const manualBitrateOptions = (codec: VideoCodec): number[] => MANUAL_OPTIONS[codec];
