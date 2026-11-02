import assert from 'node:assert/strict';

import { test } from 'vite-plus/test';

import {
  manualBitrateOptions,
  recommendBitrateCap,
  recommendedBitrateRange,
} from '../../../apps/desktop/src/renderer/utils/bitrate.ts';

test('recommendBitrateCap picks the static 1080p60 AV1 sweet spot (8 Mbps)', () => {
  assert.equal(recommendBitrateCap({ codec: 'av1', fps: 60, motionTier: 'static', resolution: '1080p' }), 8_000_000);
});

test('recommendBitrateCap picks the 1440p60 AV1 sweet spot (12 Mbps)', () => {
  assert.equal(recommendBitrateCap({ codec: 'av1', fps: 60, motionTier: 'static', resolution: '1440p' }), 12_000_000);
});

test('recommendBitrateCap scales AV1 up with dynamic motion', () => {
  assert.equal(recommendBitrateCap({ codec: 'av1', fps: 60, motionTier: 'dynamic', resolution: '1080p' }), 12_000_000);
  assert.equal(recommendBitrateCap({ codec: 'av1', fps: 60, motionTier: 'dynamic', resolution: '1440p' }), 18_000_000);
});

test('recommendBitrateCap gives H.264 a higher ceiling than AV1 at the same resolution', () => {
  const h264 = recommendBitrateCap({
    codec: 'h264',
    fps: 60,
    motionTier: 'static',
    resolution: '1080p',
  });
  const av1 = recommendBitrateCap({
    codec: 'av1',
    fps: 60,
    motionTier: 'static',
    resolution: '1080p',
  });
  assert.ok(h264 > av1);
});

test('recommendBitrateCap treats H.265 like VP9 (same efficiency class)', () => {
  const h265 = recommendBitrateCap({
    codec: 'h265',
    fps: 60,
    motionTier: 'static',
    resolution: '1080p',
  });
  const vp9 = recommendBitrateCap({
    codec: 'vp9',
    fps: 60,
    motionTier: 'static',
    resolution: '1080p',
  });
  const av1 = recommendBitrateCap({
    codec: 'av1',
    fps: 60,
    motionTier: 'static',
    resolution: '1080p',
  });
  assert.equal(h265, vp9);
  assert.ok(h265 > av1);
  assert.ok(manualBitrateOptions('h265').length > 0);
});

test('recommendBitrateCap scales down for 30 fps', () => {
  const sixty = recommendBitrateCap({
    codec: 'av1',
    fps: 60,
    motionTier: 'static',
    resolution: '1080p',
  });
  const thirty = recommendBitrateCap({
    codec: 'av1',
    fps: 30,
    motionTier: 'static',
    resolution: '1080p',
  });
  assert.ok(thirty < sixty);
});

test('recommendBitrateCap snaps to an allowed option', () => {
  const cap = recommendBitrateCap({
    codec: 'av1',
    fps: 60,
    motionTier: 'static',
    resolution: '1080p',
  });
  assert.ok(manualBitrateOptions('av1').includes(cap));
});

test('recommendedBitrateRange gives 1080p60 H.264 an 8–12 Mbps band', () => {
  assert.deepEqual(
    recommendedBitrateRange({ codec: 'h264', fps: 60, motionTier: 'static', resolution: '1080p' }),
    [8_000_000, 12_000_000],
  );
});

test('manualBitrateOptions keeps AV1 below the H.264 20+ Mbps band', () => {
  assert.ok(manualBitrateOptions('av1').every((v) => v <= 20_000_000));
});
