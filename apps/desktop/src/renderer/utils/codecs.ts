import type { VideoCodec } from '@slopcast/shared-types';
import { VIDEO_CODEC_PRIORITY } from '@slopcast/shared-types';
import type { NativeCodecInfo } from '../types';

export interface CodecInfo {
  codec: VideoCodec;
  label: string;
  hardware: boolean;
  recommended: boolean;
}

// Preference order from `VIDEO_CODEC_PRIORITY` (shared-types): h264 first —
// it is the only hardware-capable codec in the native stack, so the picker
// shows it on top.
export const sortByCodecPreference = (codecs: CodecInfo[]): CodecInfo[] => {
  const priority = new Map<VideoCodec, number>(VIDEO_CODEC_PRIORITY.map((c, i) => [c, i]));
  return [...codecs].sort((a, b) => (priority.get(a.codec) ?? 99) - (priority.get(b.codec) ?? 99));
};

// The native stack (bundled libwebrtc in native-livekit) is the ONLY encoder
// this app uses — the webview's `RTCRtpSender.getCapabilities` reflects the
// WebKitGTK/GStreamer stack, which never touches the stream, so it must never
// drive the codec picker. `hardware` comes from the native build: only H264
// has a hardware encoder factory (VA-API/NVENC on Linux, Media Foundation on
// Windows, VideoToolbox on macOS); VP8/VP9/AV1 are always software there.
export const fromNativeCodecInfo = (infos: NativeCodecInfo[]): CodecInfo[] => {
  const codecs = infos
    .filter((i): i is NativeCodecInfo & { codec: VideoCodec } => ['vp8', 'h264', 'vp9', 'av1'].includes(i.codec))
    .map((i) => ({ codec: i.codec, label: i.label, hardware: i.hardware, recommended: false }));
  return sortByCodecPreference(codecs);
};

// Hoists the recommended choice: the first hardware-capable codec, or H.264
// when no hardware encoder exists (H.264 is the only software codec the
// picker should ever fall back to — libvpx VP9 and libaom AV1 cannot sustain
// real-time 1080p60 in software).
export const recommendCodec = (codecs: CodecInfo[]): CodecInfo[] => {
  if (codecs.length === 0) return [];
  const recommended = codecs.find((c) => c.hardware) ?? codecs.find((c) => c.codec === 'h264');
  if (!recommended) return codecs;
  return [{ ...recommended, recommended: true }, ...codecs.filter((c) => c.codec !== recommended.codec)];
};

export const codecOptionSuffix = (info: CodecInfo): string => {
  if (info.recommended) return info.hardware ? 'Hardware - Recommended' : 'Recommended';
  if (info.hardware) return 'Hardware';
  // libaom (AV1) is single-threaded and libvpx VP9 is too heavy to sustain
  // real-time 1080p60 in software; calling that out prevents pickers from
  // choosing a codec the encoder cannot keep up with.
  if (info.codec === 'av1' || info.codec === 'vp9') return 'Software (slow)';
  return 'Software';
};
