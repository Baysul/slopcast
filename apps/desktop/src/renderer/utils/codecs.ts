import type { VideoCodec } from '@slopcast/shared-types';
import { DEFAULT_STREAM_SETTINGS, VIDEO_CODEC_PRIORITY } from '@slopcast/shared-types';
import type { NativeCodecInfo } from '../types';

export interface CodecInfo {
  codec: VideoCodec;
  label: string;
  hardware: boolean;
  recommended: boolean;
}

// Preference order from `VIDEO_CODEC_PRIORITY` (shared-types): h264 first,
// then h265 — the hardware-capable codecs — so the picker shows them on
// top.
export const sortByCodecPreference = (codecs: CodecInfo[]): CodecInfo[] => {
  const priority = new Map<VideoCodec, number>(VIDEO_CODEC_PRIORITY.map((c, i) => [c, i]));
  return [...codecs].sort((a, b) => (priority.get(a.codec) ?? 99) - (priority.get(b.codec) ?? 99));
};

// The native stack (bundled libwebrtc in native-livekit) is the ONLY encoder
// this app uses — the webview's `RTCRtpSender.getCapabilities` reflects the
// WebKitGTK/GStreamer stack, which never touches the stream, so it must never
// drive the codec picker. `hardware` and `label` come from
// `get_native_supported_codecs` at runtime: the native stack probes its
// encoder chain (NVENC → VA-API → software on Linux) and reports both the
// winning encoder's hardware flag and its vendor suffix ("H.264 (NVENC)"),
// so the renderer never hardcodes a codec → encoder mapping.
export const fromNativeCodecInfo = (infos: NativeCodecInfo[]): CodecInfo[] => {
  const codecs = infos
    .filter((i): i is NativeCodecInfo & { codec: VideoCodec } =>
      ['vp8', 'h264', 'h265', 'vp9', 'av1'].includes(i.codec),
    )
    .map((i) => ({ codec: i.codec, label: i.label, hardware: i.hardware, recommended: false }));
  return sortByCodecPreference(codecs);
};

// Hoists the recommended choice: a hardware H.264 when one is present (the
// fastest encode path on NVENC/VA-API/Media Foundation/VideoToolbox boxes),
// otherwise the shipped default codec (vp8 — see DEFAULT_STREAM_SETTINGS).
// A saved setting always beats the recommendation: the persisted codec is
// hydrated first and the recommendation only fills a fresh session.
export const recommendCodec = (codecs: CodecInfo[]): CodecInfo[] => {
  const recommended =
    codecs.find((c) => c.hardware && c.codec === 'h264') ??
    codecs.find((c) => c.codec === DEFAULT_STREAM_SETTINGS.videoCodec) ??
    codecs.at(0);
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
