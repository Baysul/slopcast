import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { NativeCodecInfo } from '../src/renderer/types/index.ts';
import {
  type CodecInfo,
  codecOptionSuffix,
  fromNativeCodecInfo,
  groupCodecsByHardware,
  recommendCodec,
  sortByCodecPreference,
} from '../src/renderer/utils/codecs.ts';

function info(codec: CodecInfo['codec'], hardware = false, recommended = false): CodecInfo {
  return { codec, label: codec.toUpperCase(), hardware, recommended };
}

test('sortByCodecPreference orders h264 > h265 > vp8 > vp9 > av1', () => {
  const sorted = sortByCodecPreference([info('av1'), info('h265'), info('h264'), info('vp9'), info('vp8')]);
  assert.deepEqual(
    sorted.map((c) => c.codec),
    ['h264', 'h265', 'vp8', 'vp9', 'av1'],
  );
});

test('sortByCodecPreference keeps unknown codecs last', () => {
  const sorted = sortByCodecPreference([info('vp8'), info('h264')]);
  assert.deepEqual(
    sorted.map((c) => c.codec),
    ['h264', 'vp8'],
  );
});

test('sortByCodecPreference does not mutate its input', () => {
  const input = [info('vp8'), info('av1')];
  sortByCodecPreference(input);
  assert.deepEqual(
    input.map((c) => c.codec),
    ['vp8', 'av1'],
  );
});

test('codecOptionSuffix marks only the recommended codec (labels carry the encoder suffix)', () => {
  assert.equal(codecOptionSuffix(info('vp9', true, true)), ' - Recommended');
  assert.equal(codecOptionSuffix(info('vp9', true, false)), '');
  assert.equal(codecOptionSuffix(info('vp9', false, true)), ' - Recommended');
  assert.equal(codecOptionSuffix(info('vp9', false, false)), '');
  assert.equal(codecOptionSuffix(info('vp8', false, false)), '');
});

test('fromNativeCodecInfo maps the native stack list and sorts by preference', () => {
  const native: NativeCodecInfo[] = [
    { codec: 'h264', label: 'H.264', hardware: true },
    { codec: 'h265', label: 'H.265', hardware: true },
    { codec: 'vp8', label: 'VP8', hardware: false },
    { codec: 'vp9', label: 'VP9', hardware: false },
    { codec: 'av1', label: 'AV1', hardware: false },
  ];
  const codecs = fromNativeCodecInfo(native);
  assert.deepEqual(
    codecs.map((c) => c.codec),
    ['h264', 'h265', 'vp8', 'vp9', 'av1'],
  );
  assert.ok(codecs.every((c) => !c.recommended));
  assert.equal(codecs.find((c) => c.codec === 'h264')?.hardware, true);
  assert.equal(codecs.find((c) => c.codec === 'h264')?.label, 'H.264');
  assert.equal(codecs.find((c) => c.codec === 'h265')?.hardware, true);
  assert.equal(codecs.find((c) => c.codec === 'h265')?.label, 'H.265');
  assert.equal(codecs.find((c) => c.codec === 'vp8')?.hardware, false);
});

test('fromNativeCodecInfo keeps h265 and drops unknown codecs', () => {
  const native: NativeCodecInfo[] = [
    { codec: 'h265', label: 'H.265', hardware: true },
    { codec: 'theora', label: 'Theora', hardware: false },
  ];
  assert.deepEqual(
    fromNativeCodecInfo(native).map((c) => c.codec),
    ['h265'],
  );
  assert.deepEqual(fromNativeCodecInfo([]), []);
});

test('recommendCodec hoists the shipped default codec (vp8) when no hardware H.264 exists', () => {
  const input = sortByCodecPreference([
    { ...info('h264', false) },
    { ...info('vp8', false) },
    { ...info('vp9', false) },
    { ...info('av1', false) },
  ]);
  const recommended = recommendCodec(input);
  assert.equal(recommended[0]?.codec, 'vp8');
  assert.equal(recommended[0]?.recommended, true);
  assert.ok(recommended.slice(1).every((c) => !c.recommended));
});

test('recommendCodec hoists hardware H.264 over the vp8 default', () => {
  const input = sortByCodecPreference([
    { ...info('h264', true) },
    { ...info('vp8', false) },
    { ...info('vp9', false) },
    { ...info('av1', false) },
  ]);
  const recommended = recommendCodec(input);
  assert.equal(recommended[0]?.codec, 'h264');
  assert.equal(recommended[0]?.recommended, true);
  assert.ok(recommended.slice(1).every((c) => !c.recommended));
});

test('recommendCodec falls back to the first available codec when vp8 is absent', () => {
  const input = sortByCodecPreference([info('vp9'), info('h264'), info('av1')]);
  const recommended = recommendCodec(input);
  assert.equal(recommended[0]?.codec, 'h264');
  assert.equal(recommended[0]?.recommended, true);
});

test('recommendCodec marks a single vp8 list as recommended (it is the shipped default)', () => {
  const input = [info('vp8')];
  const recommended = recommendCodec(input);
  assert.deepEqual(recommended, [{ ...info('vp8'), recommended: true }]);
});

test('recommendCodec handles a single non-default codec without crashing', () => {
  const input = [info('h264', true)];
  const recommended = recommendCodec(input);
  assert.deepEqual(recommended, [{ ...info('h264', true), recommended: true }]);
});

test('recommendCodec returns an empty list unchanged', () => {
  assert.deepEqual(recommendCodec([]), []);
});

test('groupCodecsByHardware puts hardware first, preserving order within each group', () => {
  const codecs = recommendCodec(
    fromNativeCodecInfo([
      { codec: 'h264', label: 'H.264 (NVENC)', hardware: true },
      { codec: 'h265', label: 'H.265 (NVENC)', hardware: true },
      { codec: 'vp8', label: 'VP8', hardware: false },
      { codec: 'vp9', label: 'VP9', hardware: false },
      { codec: 'av1', label: 'AV1', hardware: false },
    ]),
  );
  const { hardware, software } = groupCodecsByHardware(codecs);
  assert.deepEqual(
    hardware.map((c) => c.codec),
    ['h264', 'h265'],
  );
  // The recommended codec (hardware h264) stays first within its group.
  assert.equal(hardware[0]?.recommended, true);
  assert.deepEqual(
    software.map((c) => c.codec),
    ['vp8', 'vp9', 'av1'],
  );
});

test('groupCodecsByHardware handles an all-software and an empty list', () => {
  assert.deepEqual(groupCodecsByHardware([info('vp8')]), { hardware: [], software: [info('vp8')] });
  assert.deepEqual(groupCodecsByHardware([]), { hardware: [], software: [] });
});
