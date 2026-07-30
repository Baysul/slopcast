import { codecLabel } from '@slopcast/shared-types';

export interface SpectatorTelemetry {
  videoCodec: string | null;
  width: number | null;
  height: number | null;
  frameRate: number | null;
  videoBitrate: number | null;
  packetLossPct: number | null;
  freezeCount: number;
  hasVideo: boolean;
  hasAudio: boolean;
  quality: 'excellent' | 'degraded' | 'poor';
  framesReceived: number;
  packetsReceived: number;
  decoderImplementation: string | null;
}

interface RTCStatLike {
  type: string;
  kind?: string;
  timestamp?: number;
  codecId?: string;
  bytesReceived?: number;
  packetsReceived?: number;
  packetsLost?: number;
  framesReceived?: number;
  framesDecoded?: number;
  frameWidth?: number;
  frameHeight?: number;
  mimeType?: string;
  implementation?: string;
  freezeCount?: number;
  totalFreezesDuration?: number;
  pauseCount?: number;
  totalPausesDuration?: number;
}

interface StatsPrev {
  bytesReceived: number;
  framesDecoded: number;
  ts: number;
  init: boolean;
}

function computeQuality(
  br: number | null,
  fps: number | null,
  loss: number | null,
  freeze: number,
): 'excellent' | 'degraded' | 'poor' {
  if (br != null && br < 500_000) return 'poor';
  if (fps != null && fps < 10) return 'poor';
  if (loss != null && loss > 3) return 'poor';
  if (freeze > 0) return 'poor';
  if (br != null && br < 2_000_000) return 'degraded';
  if (fps != null && fps < 20) return 'degraded';
  if (loss != null && loss > 0.5) return 'degraded';
  return 'excellent';
}

export function computeTelemetry(stats: RTCStatsReport, prev: StatsPrev | null, hasAudio: boolean): SpectatorTelemetry {
  let videoCodec: string | null = null;
  let width: number | null = null;
  let height: number | null = null;
  let frameRate: number | null = null;
  let videoBitrate: number | null = null;
  let packetLossPct: number | null = null;
  let freezeCount = 0;
  let hasVideo = false;
  let framesReceived = 0;
  let packetsReceived = 0;
  let decoderImplementation: string | null = null;

  for (const reportRaw of stats.values()) {
    const report = reportRaw as RTCStatLike;
    if (report.type === 'inbound-rtp' && report.kind === 'video') {
      hasVideo = true;
      const ts = report.timestamp ?? 0;

      if (report.codecId) {
        const codec = stats.get(report.codecId) as RTCStatLike | undefined;
        videoCodec = codecLabel(codec?.mimeType);
      }

      width = report.frameWidth ?? width;
      height = report.frameHeight ?? height;

      if (prev?.init && ts > prev.ts) {
        const dt = (ts - prev.ts) / 1000;
        const db = (report.bytesReceived ?? 0) - prev.bytesReceived;
        if (db >= 0) videoBitrate = (db * 8) / dt;
        const df = (report.framesDecoded ?? 0) - prev.framesDecoded;
        if (df >= 0) frameRate = df / dt;
      }

      const totalReceived = (report.packetsReceived ?? 0) + (report.packetsLost ?? 0);
      if (totalReceived > 0) {
        packetLossPct = ((report.packetsLost ?? 0) / totalReceived) * 100;
      }

      freezeCount = report.freezeCount ?? 0;
      framesReceived = report.framesDecoded ?? 0;
      packetsReceived = report.packetsReceived ?? 0;
    }

    if (report.type === 'codec' && report.mimeType?.toUpperCase()?.includes('VIDEO')) {
      if (report.implementation) {
        decoderImplementation = report.implementation;
      }
    }
  }

  const quality = computeQuality(videoBitrate, frameRate, packetLossPct, freezeCount);

  return {
    videoCodec,
    width,
    height,
    frameRate,
    videoBitrate,
    packetLossPct,
    freezeCount,
    hasVideo,
    hasAudio,
    quality,
    framesReceived,
    packetsReceived,
    decoderImplementation,
  };
}

export function createStatsPrev(stats: RTCStatsReport): StatsPrev | null {
  for (const reportRaw of stats.values()) {
    const report = reportRaw as RTCStatLike;
    if (report.type === 'inbound-rtp' && report.kind === 'video') {
      return {
        bytesReceived: report.bytesReceived ?? 0,
        framesDecoded: report.framesDecoded ?? 0,
        ts: report.timestamp ?? 0,
        init: true,
      };
    }
  }
  return null;
}
