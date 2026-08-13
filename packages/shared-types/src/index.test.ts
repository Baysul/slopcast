import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  codecLabel,
  DEFAULT_STREAM_SETTINGS,
  fmtBitrate,
  fmtLoss,
  normalizeLivekitUrl,
  sanitizeStreamSettings,
} from './index.js';

// sanitizeStreamSettings: corrupted or hand-edited files must never
// crash the app; every field falls back to a default individually.

test('non-object input yields the defaults (defensive copy)', () => {
  for (const raw of [null, undefined, 42, 'fps:60', [], true]) {
    assert.deepEqual(sanitizeStreamSettings(raw), DEFAULT_STREAM_SETTINGS);
  }
});

test('the returned defaults are a fresh copy, not the shared constant', () => {
  const settings = sanitizeStreamSettings(null);
  settings.fps = 999;
  assert.equal(DEFAULT_STREAM_SETTINGS.fps, 60);
});

test('valid fields pass through unchanged', () => {
  const raw = {
    fps: 30,
    bitrateLimit: 5_000_000,
    videoCodec: 'h264',
    resolution: '720p',
    apiEndpoint: 'https://srv.example.com',
    autoBitrate: false,
    motionMode: 'dynamic',
  };
  assert.deepEqual(sanitizeStreamSettings(raw), raw);
});

test('out-of-range fps and bitrate fall back', () => {
  assert.equal(sanitizeStreamSettings({ fps: 0 }).fps, DEFAULT_STREAM_SETTINGS.fps);
  assert.equal(sanitizeStreamSettings({ fps: 241 }).fps, DEFAULT_STREAM_SETTINGS.fps);
  assert.equal(sanitizeStreamSettings({ fps: -5 }).fps, DEFAULT_STREAM_SETTINGS.fps);
  assert.equal(sanitizeStreamSettings({ bitrateLimit: 99_999 }).bitrateLimit, DEFAULT_STREAM_SETTINGS.bitrateLimit);
  assert.equal(
    sanitizeStreamSettings({ bitrateLimit: 200_000_001 }).bitrateLimit,
    DEFAULT_STREAM_SETTINGS.bitrateLimit,
  );
});

test('non-numeric and non-finite numbers fall back', () => {
  assert.equal(sanitizeStreamSettings({ fps: '60' }).fps, DEFAULT_STREAM_SETTINGS.fps);
  assert.equal(sanitizeStreamSettings({ fps: Number.NaN }).fps, DEFAULT_STREAM_SETTINGS.fps);
  assert.equal(sanitizeStreamSettings({ fps: Number.POSITIVE_INFINITY }).fps, DEFAULT_STREAM_SETTINGS.fps);
  // Boundary values themselves are accepted (fps is capped at 60 — the
  // capture pacer and preview emitter clamp there regardless, and higher
  // values would run 60 fps with a 120 fps SDP claim).
  assert.equal(sanitizeStreamSettings({ fps: 1 }).fps, 1);
  assert.equal(sanitizeStreamSettings({ fps: 240 }).fps, 60);
  assert.equal(sanitizeStreamSettings({ fps: 60 }).fps, 60);
});

test('unknown codec and resolution strings fall back', () => {
  assert.equal(sanitizeStreamSettings({ videoCodec: 'theora' }).videoCodec, 'vp8');
  assert.equal(sanitizeStreamSettings({ videoCodec: 'H264' }).videoCodec, 'vp8');
  assert.equal(sanitizeStreamSettings({ resolution: '4k' }).resolution, '1080p');
  assert.equal(sanitizeStreamSettings({ resolution: '720P' }).resolution, '1080p');
});

test('autoBitrate and motionMode fall back on invalid values', () => {
  assert.equal(sanitizeStreamSettings({ autoBitrate: 'yes' }).autoBitrate, true);
  assert.equal(sanitizeStreamSettings({ autoBitrate: false }).autoBitrate, false);
  assert.equal(sanitizeStreamSettings({ motionMode: 'gaming' }).motionMode, 'auto');
  assert.equal(sanitizeStreamSettings({ motionMode: 'dynamic' }).motionMode, 'dynamic');
});

test('empty or non-string apiEndpoint falls back', () => {
  assert.equal(sanitizeStreamSettings({ apiEndpoint: '' }).apiEndpoint, 'http://localhost:3001');
  assert.equal(sanitizeStreamSettings({ apiEndpoint: '   ' }).apiEndpoint, 'http://localhost:3001');
  assert.equal(sanitizeStreamSettings({ apiEndpoint: 42 }).apiEndpoint, 'http://localhost:3001');
});

test('apiEndpoint is kept verbatim (not trimmed or normalized)', () => {
  // Documenting the current contract: the value is stored as given.
  assert.equal(sanitizeStreamSettings({ apiEndpoint: '  http://x  ' }).apiEndpoint, '  http://x  ');
});

test('fmtBitrate formats null as an em dash', () => {
  assert.equal(fmtBitrate(null), '\u2014');
});

test('fmtBitrate switches units at 1 Mbps', () => {
  assert.equal(fmtBitrate(1_000_000), '1.0 Mbps');
  assert.equal(fmtBitrate(12_345_678), '12.3 Mbps');
  assert.equal(fmtBitrate(999_999), '1000 kbps');
  assert.equal(fmtBitrate(0), '1 kbps');
});

test('fmtLoss handles null, sub-0.1 and normal values', () => {
  assert.equal(fmtLoss(null), '—');
  assert.equal(fmtLoss(0.05), '0.05%');
  assert.equal(fmtLoss(0.123), '0.1%');
  assert.equal(fmtLoss(12.345), '12.3%');
});

test('codecLabel maps known mime types and falls back to stripped mime', () => {
  assert.equal(codecLabel('VIDEO/H264'), 'H.264');
  assert.equal(codecLabel('video/av1'), 'AV1');
  assert.equal(codecLabel('AUDIO/OPUS'), 'Opus');
  assert.equal(codecLabel('VIDEO/WEIRD'), 'WEIRD');
  assert.equal(codecLabel(null), null);
  assert.equal(codecLabel(undefined), null);
});

// normalizeLivekitUrl: ws:// URLs are mixed content on HTTPS pages and must
// upgrade to wss://; plain HTTP/localhost dev keeps ws://.

test('normalizeLivekitUrl upgrades ws:// to wss:// on HTTPS pages', () => {
  assert.equal(normalizeLivekitUrl('ws://livekit.example.com:7880', true), 'wss://livekit.example.com:7880');
});

test('normalizeLivekitUrl leaves wss:// untouched on HTTPS pages', () => {
  assert.equal(normalizeLivekitUrl('wss://livekit.example.com:7880', true), 'wss://livekit.example.com:7880');
});

test('normalizeLivekitUrl leaves ws:// untouched on plain HTTP pages', () => {
  assert.equal(normalizeLivekitUrl('ws://localhost:7880', false), 'ws://localhost:7880');
});

test('normalizeLivekitUrl leaves wss:// untouched on plain HTTP pages', () => {
  assert.equal(normalizeLivekitUrl('wss://localhost:7880', false), 'wss://localhost:7880');
});

test('normalizeLivekitUrl passes non-ws URLs through', () => {
  assert.equal(normalizeLivekitUrl('http://livekit.example.com:7880', true), 'http://livekit.example.com:7880');
  assert.equal(normalizeLivekitUrl('', true), '');
});
