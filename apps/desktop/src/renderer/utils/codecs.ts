import type { VideoCodec } from '@slopcast/shared-types';
import { DEFAULT_STREAM_SETTINGS, VIDEO_CODEC_PRIORITY } from '@slopcast/shared-types';
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
// drive the codec picker. `hardware` comes from `get_native_supported_codecs`
// at runtime: the bundled libwebrtc currently ships a hardware encoder factory
// only for H264 (VA-API on Linux, Media Foundation on Windows, VideoToolbox on
// macOS), so today VP8/VP9/AV1 report software — but labels always follow the
// reported flag, never a hardcoded codec → encoder mapping.
export const fromNativeCodecInfo = (infos: NativeCodecInfo[]): CodecInfo[] => {
  const codecs = infos
    .filter((i): i is NativeCodecInfo & { codec: VideoCodec } => ['vp8', 'h264', 'vp9', 'av1'].includes(i.codec))
    .map((i) => ({ codec: i.codec, label: i.label, hardware: i.hardware, recommended: false }));
  return sortByCodecPreference(codecs);
};

// Hoists the recommended choice: the shipped default codec (vp8 — see
// DEFAULT_STREAM_SETTINGS; VA-API H264 collapses to ~1-3 fps on Linux).
export const recommendCodec = (codecs: CodecInfo[]): CodecInfo[] => {
  const recommended = codecs.find((c) => c.codec === DEFAULT_STREAM_SETTINGS.videoCodec) ?? codecs.at(0);
  if (!recommended) return [];

  return [{ ...recommended, recommended: true }, ...codecs.filter((c) => c.codec !== recommended.codec)];
};

// Only the recommended codec gets a suffix — hardware vs. software is already
// conveyed by the picker's group labels, and the `hardware` flag itself comes
// from `get_native_supported_codecs` at list-generation time.
export const codecOptionSuffix = (info: CodecInfo): string => (info.recommended ? ' - Recommended' : '');

// Partitions codecs for the grouped picker (hardware group on top), preserving
// the preference order within each partition so the recommendation stays first
// in its group.
export const groupCodecsByHardware = (codecs: CodecInfo[]): { hardware: CodecInfo[]; software: CodecInfo[] } => ({
  hardware: codecs.filter((c) => c.hardware),
  software: codecs.filter((c) => !c.hardware),
});
