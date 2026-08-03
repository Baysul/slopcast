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
  framesDecoded: number;
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
  packetsReceived: number;
  packetsLost: number;
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

interface VideoStats {
  videoCodec: string | null;
  width: number | null;
  height: number | null;
  frameRate: number | null;
  videoBitrate: number | null;
  packetLossPct: number | null;
  freezeCount: number;
  hasVideo: boolean;
  framesDecoded: number;
  packetsReceived: number;
}

const applyVideoDelta = (acc: VideoStats, report: RTCStatLike, prev: StatsPrev): void => {
  const ts = report.timestamp ?? 0;
  if (!prev.init || ts <= prev.ts) return;
  const dt = (ts - prev.ts) / 1000;
  const db = (report.bytesReceived ?? 0) - prev.bytesReceived;
  if (db >= 0) acc.videoBitrate = (db * 8) / dt;
  const df = (report.framesDecoded ?? 0) - prev.framesDecoded;
  if (df >= 0) acc.frameRate = df / dt;
  // Delta-based loss: cumulative loss understates recent degradation
  // and can never recover after a burst.
  const dl = (report.packetsLost ?? 0) - prev.packetsLost;
  const dr = (report.packetsReceived ?? 0) - prev.packetsReceived;
  const total = dl + dr;
  if (total > 0) {
    acc.packetLossPct = (dl / total) * 100;
  }
};

const foldInboundVideo = (
  acc: VideoStats,
  report: RTCStatLike,
  stats: RTCStatsReport,
  prev: StatsPrev | null,
): void => {
  const ts = report.timestamp ?? 0;

  if (report.codecId) {
    const codec = stats.get(report.codecId) as RTCStatLike | undefined;
    acc.videoCodec = codecLabel(codec?.mimeType);
  }

  acc.width = report.frameWidth ?? acc.width;
  acc.height = report.frameHeight ?? acc.height;

  if (prev?.init && ts > prev.ts) {
    applyVideoDelta(acc, report, prev);
  } else {
    const totalReceived = (report.packetsReceived ?? 0) + (report.packetsLost ?? 0);
    if (totalReceived > 0) {
      acc.packetLossPct = ((report.packetsLost ?? 0) / totalReceived) * 100;
    }
  }

  acc.freezeCount = report.freezeCount ?? 0;
  acc.framesDecoded = report.framesDecoded ?? 0;
  acc.packetsReceived = report.packetsReceived ?? 0;
};

export function computeTelemetry(stats: RTCStatsReport, prev: StatsPrev | null, hasAudio: boolean): SpectatorTelemetry {
  const video: VideoStats = {
    videoCodec: null,
    width: null,
    height: null,
    frameRate: null,
    videoBitrate: null,
    packetLossPct: null,
    freezeCount: 0,
    hasVideo: false,
    framesDecoded: 0,
    packetsReceived: 0,
  };
  let decoderImplementation: string | null = null;

  for (const reportRaw of stats.values()) {
    const report = reportRaw as RTCStatLike;
    if (report.type === 'inbound-rtp' && report.kind === 'video') {
      video.hasVideo = true;
      foldInboundVideo(video, report, stats, prev);
    }

    if (report.type === 'codec' && report.mimeType?.toUpperCase()?.includes('VIDEO')) {
      if (report.implementation) {
        decoderImplementation = report.implementation;
      }
    }
  }

  const quality = computeQuality(video.videoBitrate, video.frameRate, video.packetLossPct, video.freezeCount);

  return {
    videoCodec: video.videoCodec,
    width: video.width,
    height: video.height,
    frameRate: video.frameRate,
    videoBitrate: video.videoBitrate,
    packetLossPct: video.packetLossPct,
    freezeCount: video.freezeCount,
    hasVideo: video.hasVideo,
    hasAudio,
    quality,
    framesDecoded: video.framesDecoded,
    packetsReceived: video.packetsReceived,
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
        packetsReceived: report.packetsReceived ?? 0,
        packetsLost: report.packetsLost ?? 0,
        ts: report.timestamp ?? 0,
        init: true,
      };
    }
  }
  return null;
}
