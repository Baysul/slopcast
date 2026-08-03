import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  type CodecInfo,
  codecOptionSuffix,
  detectSupportedCodecs,
  probeCodecHardware,
  sortByEncodingEfficiency,
  supportsHardwareEncoding,
} from '../src/renderer/utils/codecs.ts';

function info(codec: CodecInfo['codec'], hardware = false, recommended = false): CodecInfo {
  return { codec, label: codec.toUpperCase(), hardware, recommended };
}

test('sortByEncodingEfficiency orders av1 > vp9 > h264 > vp8', () => {
  const sorted = sortByEncodingEfficiency([info('h264'), info('av1'), info('vp8'), info('vp9')]);
  assert.deepEqual(
    sorted.map((c) => c.codec),
    ['av1', 'vp9', 'h264', 'vp8'],
  );
});

test('sortByEncodingEfficiency keeps unknown codecs last', () => {
  const sorted = sortByEncodingEfficiency([info('vp8'), info('h264')]);
  assert.deepEqual(
    sorted.map((c) => c.codec),
    ['h264', 'vp8'],
  );
});

test('sortByEncodingEfficiency does not mutate its input', () => {
  const input = [info('vp8'), info('av1')];
  sortByEncodingEfficiency(input);
  assert.deepEqual(
    input.map((c) => c.codec),
    ['vp8', 'av1'],
  );
});

test('codecOptionSuffix labels hardware and recommended states', () => {
  assert.equal(codecOptionSuffix(info('vp9', true, true)), 'Hardware - Recommended');
  assert.equal(codecOptionSuffix(info('vp9', true, false)), 'Hardware');
  assert.equal(codecOptionSuffix(info('vp9', false, false)), 'Software');
});

test('detectSupportedCodecs falls back to h264 when capabilities are unavailable', () => {
  Object.assign(globalThis, { RTCRtpSender: { getCapabilities: () => null } });
  const codecs = detectSupportedCodecs();
  assert.deepEqual(codecs, [{ codec: 'h264', label: 'H.264', hardware: false, recommended: false }]);
});

test('detectSupportedCodecs maps mime types once per codec family', () => {
  Object.assign(globalThis, {
    RTCRtpSender: {
      getCapabilities: () => ({
        codecs: [
          { mimeType: 'video/VP9' },
          { mimeType: 'video/VP8' },
          // Duplicate family entries must collapse to one entry.
          { mimeType: 'video/VP8' },
          { mimeType: 'video/AV1' },
          { mimeType: 'audio/opus' },
        ],
      }),
    },
  });
  const codecs = detectSupportedCodecs();
  assert.deepEqual(
    codecs.map((c) => c.codec),
    ['av1', 'vp9', 'vp8'],
  );
});

test('supportsHardwareEncoding succeeds on the first supported probe', async () => {
  let probes = 0;
  Object.assign(globalThis, {
    VideoEncoder: {
      isConfigSupported: async () => {
        probes += 1;
        return { supported: probes === 1 };
      },
    },
  });
  assert.equal(await supportsHardwareEncoding('vp8'), true);
  assert.equal(probes, 1);
});

test('supportsHardwareEncoding probes every variant before giving up', async () => {
  let probes = 0;
  Object.assign(globalThis, {
    VideoEncoder: {
      isConfigSupported: async () => {
        probes += 1;
        return { supported: false };
      },
    },
  });
  // h264 has 3 probe variants.
  assert.equal(await supportsHardwareEncoding('h264'), false);
  assert.equal(probes, 3);
});

test('supportsHardwareEncoding swallows probe errors and continues', async () => {
  Object.assign(globalThis, {
    VideoEncoder: {
      isConfigSupported: async () => {
        throw new Error('encoder gone');
      },
    },
  });
  assert.equal(await supportsHardwareEncoding('av1'), false);
});

test('probeCodecHardware hoists the recommended hardware codec first', async () => {
  Object.assign(globalThis, {
    VideoEncoder: { isConfigSupported: async () => ({ supported: true }) },
  });
  const input = sortByEncodingEfficiency([info('vp9'), info('h264'), info('av1')]);
  const probed = await probeCodecHardware(input);
  // Priority order is av1 > vp9 > h264; with every codec hardware, av1 is
  // recommended and hoisted to the front.
  assert.deepEqual(
    probed.map((c) => c.codec),
    ['av1', 'vp9', 'h264'],
  );
  assert.equal(probed[0]?.recommended, true);
  assert.ok(probed.slice(1).every((c) => !c.recommended));
});

test('probeCodecHardware leaves order unchanged when nothing is hardware', async () => {
  Object.assign(globalThis, {
    VideoEncoder: { isConfigSupported: async () => ({ supported: false }) },
  });
  const input = [info('h264'), info('vp8')];
  const probed = await probeCodecHardware(input);
  assert.deepEqual(
    probed.map((c) => c.codec),
    ['h264', 'vp8'],
  );
  assert.ok(probed.every((c) => !c.hardware && !c.recommended));
});
