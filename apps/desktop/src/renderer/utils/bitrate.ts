import type { ResolutionPreset, VideoCodec } from '@slopcast/shared-types';

// The auto-bitrate algorithm derives a sensible bitrate ceiling (bits/sec)
// from the codec, resolution, framerate, and content motion. It is enforced
// by default but can be toggled off to expose the manual per-codec bitrate
// dropdown.

/** Content motion class. `auto` resolves to a detected tier at runtime. */
export type MotionMode = 'auto' | 'static' | 'mixed' | 'dynamic';
/** The resolved motion tier fed into the algorithm (auto is already resolved). */
export type MotionTier = Exclude<MotionMode, 'auto'>;

// Validated software-AV1 ceilings (bits/sec) at 60 fps for static desktop
// content. These are the "sweet spots" the SVT-AV1 VBR tuning targets:
// svtav1enc runs VBR with the ceiling as `max-bitrate` and 80% of it as
// `target-bitrate`, so 1080p60 sits at 8 Mbps and 1440p60 at 12 Mbps.
const AV1_SOFTWARE_CEILING_BPS: Record<ResolutionPreset, number> = {
  '480p': 2_000_000,
  '720p': 4_000_000,
  '1080p': 8_000_000,
  '1440p': 12_000_000,
  '2160p': 20_000_000,
};

// Relative ceiling scale per codec (AV1 = 1.0). The older codecs need more
// bits for the same quality: VP9 ~1.4x, VP8 ~1.7x, H.264 ~1.5x AV1's ceiling.
// H.265 sits in the same efficiency class as VP9 (~1.4x).
const CODEC_SCALE: Record<VideoCodec, number> = {
  av1: 1.0,
  vp9: 1.4,
  h265: 1.4,
  vp8: 1.7,
  h264: 1.5,
};

// Motion multiplies the ceiling: high-motion content (gaming, full-motion
// video) needs substantially more bits to avoid blocky artifacts. The ~1.5x
// gaming factor matches the bits-per-pixel gap between static desktop
// content and fast-moving gameplay observed across encoder guides.
const MOTION_FACTOR: Record<MotionTier, number> = {
  static: 1.0,
  mixed: 1.25,
  dynamic: 1.5,
};

// Per-codec manual option lists (bits/sec). AV1 caps well below H.264's
// 20+ Mbps band because AV1 delivers equivalent quality at ~half the rate.
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

// Frame-rate scaling: 60 fps is the ceiling baseline; lower rates carry less
// data and scale down (~0.8x at 30 fps, matching the H.264 ladder ratios).
const fpsScale = (fps: number): number => {
  const clamped = Math.max(1, Math.min(60, fps));
  return 0.6 + 0.4 * (clamped / 60);
};

/**
 * Computes a balanced bitrate ceiling (bits/sec) — the "sweet spot" between
 * quality and throughput — for the given encoder and content characteristics.
 */
export const recommendBitrateCap = (input: BitrateInput): number => {
  const base = AV1_SOFTWARE_CEILING_BPS[input.resolution];
  const scale = CODEC_SCALE[input.codec] * MOTION_FACTOR[input.motionTier];
  const raw = base * fpsScale(input.fps) * scale;

  return Math.max(1_000_000, Math.round(raw / 500_000) * 500_000);
};

/**
 * Manual bitrate options for a codec, used by the settings dropdown when
 * automatic bitrate is disabled.
 */
export const manualBitrateOptions = (codec: VideoCodec): number[] => MANUAL_OPTIONS[codec];
