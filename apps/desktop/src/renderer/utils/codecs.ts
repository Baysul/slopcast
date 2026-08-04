import type { VideoCodec } from '@slopcast/shared-types';
import { VIDEO_CODEC_PRIORITY } from '@slopcast/shared-types';

export interface CodecInfo {
  codec: VideoCodec;
  label: string;
  hardware: boolean;
  recommended: boolean;
}

const KNOWN_VIDEO_CODECS: Record<string, { codec: VideoCodec; label: string }> = {
  'VIDEO/AV1': { codec: 'av1', label: 'AV1' },
  'VIDEO/H264': { codec: 'h264', label: 'H.264' },
  'VIDEO/VP9': { codec: 'vp9', label: 'VP9' },
  'VIDEO/VP8': { codec: 'vp8', label: 'VP8' },
};

// Multiple profile/level variants per family: isConfigSupported validates the
// codec string's level against the requested resolution, so probing a single
// low-level string (e.g. avc1.42E01E at 1080p) falsely reports "unsupported"
// even when the hardware encoder handles the family at higher levels.
const WEBCODECS_PROBE_CODECS: Record<VideoCodec, string[]> = {
  vp8: ['vp8'],
  h264: ['avc1.640028', 'avc1.4D4028', 'avc1.42E028'],
  vp9: ['vp09.00.40.08', 'vp09.00.41.08'],
  av1: ['av01.0.08M.08', 'av01.0.09M.08'],
};

export const sortByEncodingEfficiency = (codecs: CodecInfo[]): CodecInfo[] => {
  const priority = new Map<VideoCodec, number>(VIDEO_CODEC_PRIORITY.map((c, i) => [c, i]));
  return [...codecs].sort((a, b) => (priority.get(a.codec) ?? 99) - (priority.get(b.codec) ?? 99));
};

// Every codec the WebRTC stack can send on this device. Hardware acceleration
// is probed separately (async) via WebCodecs. WebKitGTK may lack
// `RTCRtpSender.getCapabilities` entirely (MIGRATION R2), so the sender API is
// guarded and the fallback list degrades to software H.264.
export function detectSupportedCodecs(): CodecInfo[] {
  const caps =
    typeof RTCRtpSender !== 'undefined' && typeof RTCRtpSender.getCapabilities === 'function'
      ? RTCRtpSender.getCapabilities('video')
      : null;
  if (!caps) {
    return [{ codec: 'h264', label: 'H.264', hardware: false, recommended: false }];
  }

  const mimeTypes = new Set(caps.codecs.map((c) => c.mimeType.toUpperCase()));
  const available: CodecInfo[] = [];
  for (const [mime, info] of Object.entries(KNOWN_VIDEO_CODECS)) {
    if (mimeTypes.has(mime) && !available.some((c) => c.codec === info.codec)) {
      available.push({ ...info, hardware: false, recommended: false });
    }
  }
  return sortByEncodingEfficiency(available);
}

export const supportsHardwareEncoding = async (codec: VideoCodec): Promise<boolean> => {
  for (const probe of WEBCODECS_PROBE_CODECS[codec]) {
    try {
      const { supported } = await VideoEncoder.isConfigSupported({
        codec: probe,
        width: 1920,
        height: 1080,
        bitrate: 6_000_000,
        framerate: 30,
        hardwareAcceleration: 'prefer-hardware',
      });
      if (supported) return true;
    } catch (err) {
      console.warn(`[Codecs] hardware probe failed for ${codec} (${probe}):`, err);
    }
  }
  return false;
};

// Tags each codec with hardware-acceleration availability, then hoists the
// most efficient hardware encoder to the top as the recommended choice.
export const probeCodecHardware = async (codecs: CodecInfo[]): Promise<CodecInfo[]> => {
  const probed = await Promise.all(
    codecs.map(async (info) => ({ ...info, hardware: await supportsHardwareEncoding(info.codec) })),
  );
  const recommended = probed.find((c) => c.hardware);
  if (!recommended) return probed;
  return [{ ...recommended, recommended: true }, ...probed.filter((c) => c.codec !== recommended.codec)];
};

export const codecOptionSuffix = (info: CodecInfo): string => {
  if (info.recommended) return 'Hardware - Recommended';
  return info.hardware ? 'Hardware' : 'Software';
};
