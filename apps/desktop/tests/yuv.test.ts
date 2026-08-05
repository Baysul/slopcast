import assert from 'node:assert/strict';
import { test } from 'node:test';

import { parseI420Frame } from '../src/renderer/utils/yuv.ts';

const b64 = (bytes: Uint8Array): string => Buffer.from(bytes).toString('base64');

function packedI420(width: number, height: number): Uint8Array {
  const yLen = width * height;
  const uvLen = (width / 2) * (height / 2);
  const bytes = new Uint8Array(yLen + 2 * uvLen);
  // Y ramp, U/V constants: content must be preserved verbatim.
  for (let i = 0; i < yLen; i++) bytes[i] = i % 256;
  bytes.fill(128, yLen, yLen + uvLen);
  bytes.fill(129, yLen + uvLen);
  return bytes;
}

test('parseI420Frame splits a valid payload into its planes', () => {
  const width = 640;
  const height = 360;
  const bytes = packedI420(width, height);
  const frame = parseI420Frame(b64(bytes), width, height);
  assert.equal(frame.width, width);
  assert.equal(frame.height, height);
  assert.equal(frame.data.length, width * height * 1.5);
  // Plane boundaries are preserved in order: Y, U, V.
  const yLen = width * height;
  const uvLen = (width / 2) * (height / 2);
  assert.equal(frame.data[yLen], 128);
  assert.equal(frame.data[yLen + uvLen], 129);
});

test('parseI420Frame rejects truncated payloads', () => {
  const width = 640;
  const height = 360;
  const bytes = packedI420(width, height);
  assert.throws(() => parseI420Frame(b64(bytes.subarray(0, bytes.length - 1)), width, height));
  assert.throws(() => parseI420Frame('', width, height));
});

test('parseI420Frame accepts odd-sized planes without rounding traps', () => {
  // The native side rounds chroma up (div_ceil); the parser must match the
  // byte count the backend computes for odd dimensions.
  const width = 641;
  const height = 361;
  const uvW = Math.ceil(width / 2);
  const uvH = Math.ceil(height / 2);
  const bytes = new Uint8Array(width * height + 2 * uvW * uvH);
  const frame = parseI420Frame(b64(bytes), width, height);
  assert.equal(frame.data.length, bytes.length);
});
