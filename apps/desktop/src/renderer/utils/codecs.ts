import type { VideoCodec } from '@slopcast/shared-types';
import { DEFAULT_STREAM_SETTINGS, VIDEO_CODEC_PRIORITY } from '@slopcast/shared-types';
import type { NativeCodecInfo } from '../types';

export interface CodecInfo {
  codec: VideoCodec;
  label: string;
  hardware: boolean;
  recommended: boolean;
}

export const sortByCodecPreference = (codecs: CodecInfo[]): CodecInfo[] => {
  const priority = new Map<VideoCodec, number>(VIDEO_CODEC_PRIORITY.map((c, i) => [c, i]));
  return [...codecs].sort((a, b) => (priority.get(a.codec) ?? 99) - (priority.get(b.codec) ?? 99));
};

export const fromNativeCodecInfo = (infos: NativeCodecInfo[]): CodecInfo[] => {
  const codecs = infos
    .filter((i): i is NativeCodecInfo & { codec: VideoCodec } =>
      ['vp8', 'h264', 'h265', 'vp9', 'av1'].includes(i.codec),
    )
    .map((i) => ({ codec: i.codec, label: i.label, hardware: i.hardware, recommended: false }));
  return sortByCodecPreference(codecs);
};

export const recommendCodec = (codecs: CodecInfo[]): CodecInfo[] => {
  const recommended =
    codecs.find((c) => c.hardware && c.codec === 'h264') ??
    codecs.find((c) => c.codec === DEFAULT_STREAM_SETTINGS.videoCodec) ??
    codecs.at(0);
  if (!recommended) return [];

  return [{ ...recommended, recommended: true }, ...codecs.filter((c) => c.codec !== recommended.codec)];
};

export const codecOptionSuffix = (info: CodecInfo): string => (info.recommended ? ' - Recommended' : '');

export const groupCodecsByHardware = (codecs: CodecInfo[]): { hardware: CodecInfo[]; software: CodecInfo[] } => ({
  hardware: codecs.filter((c) => c.hardware),
  software: codecs.filter((c) => !c.hardware),
});
