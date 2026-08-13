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
  framesReceived: number;
  framesRendered: number;
  framesDropped: number;
  packetsDiscarded: number;
  jitterMs: number | null;
  jitterBufferDelayMs: number | null;
  jitterBufferTargetDelayMs: number | null;
  decodeTimeMs: number | null;
  processingTimeMs: number | null;
  nackCount: number;
  pliCount: number;
  firCount: number;
  retransmittedPacketsReceived: number;
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
  framesDropped?: number;
  frameWidth?: number;
  frameHeight?: number;
  mimeType?: string;
  implementation?: string;
  freezeCount?: number;
  totalFreezesDuration?: number;
  pauseCount?: number;
  totalPausesDuration?: number;
  framesRendered?: number;
  packetsDiscarded?: number;
  jitter?: number;
  jitterBufferDelay?: number;
  jitterBufferTargetDelay?: number;
  jitterBufferEmittedCount?: number;
  totalDecodeTime?: number;
  totalProcessingDelay?: number;
  nackCount?: number;
  pliCount?: number;
  firCount?: number;
  retransmittedPacketsReceived?: number;
  ssrc?: number;
  rid?: string;
}

interface StatsPrev {
  bytesReceived: number;
  packetsReceived: number;
  packetsLost: number;
  framesReceived: number;
  framesDecoded: number;
  framesRendered: number;
  framesDropped: number;
  packetsDiscarded: number;
  jitterBufferDelay: number;
  jitterBufferTargetDelay: number;
  jitterBufferEmittedCount: number;
  totalDecodeTime: number;
  totalProcessingDelay: number;
  nackCount: number;
  pliCount: number;
  firCount: number;
  retransmittedPacketsReceived: number;
  ssrc?: number;
  rid?: string;
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
  framesReceived: number;
  framesRendered: number;
  framesDropped: number;
  packetsDiscarded: number;
  jitterMs: number | null;
  jitterBufferDelayMs: number | null;
  jitterBufferTargetDelayMs: number | null;
  decodeTimeMs: number | null;
  processingTimeMs: number | null;
  nackCount: number;
  pliCount: number;
  firCount: number;
  retransmittedPacketsReceived: number;
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
  const dl = Math.max(0, (report.packetsLost ?? 0) - prev.packetsLost);
  const dr = Math.max(0, (report.packetsReceived ?? 0) - prev.packetsReceived);
  const total = dl + dr;
  if (total > 0) {
    acc.packetLossPct = (dl / total) * 100;
  }
  const positiveDelta = (value: number | undefined, previous: number): number => Math.max(0, (value ?? 0) - previous);
  acc.framesReceived = positiveDelta(report.framesReceived, prev.framesReceived);
  acc.framesRendered = positiveDelta(report.framesRendered, prev.framesRendered);
  acc.framesDropped = positiveDelta(report.framesDropped, prev.framesDropped);
  acc.packetsDiscarded = positiveDelta(report.packetsDiscarded, prev.packetsDiscarded);
  acc.decodeTimeMs = positiveDelta(report.totalDecodeTime, prev.totalDecodeTime) * 1000;
  acc.processingTimeMs = positiveDelta(report.totalProcessingDelay, prev.totalProcessingDelay) * 1000;
  acc.nackCount = positiveDelta(report.nackCount, prev.nackCount);
  acc.pliCount = positiveDelta(report.pliCount, prev.pliCount);
  acc.firCount = positiveDelta(report.firCount, prev.firCount);
  acc.retransmittedPacketsReceived = positiveDelta(
    report.retransmittedPacketsReceived,
    prev.retransmittedPacketsReceived,
  );
};

// The stats schema has many optional counters, and folding them in one pass
// keeps the per-report semantics explicit.
// biome-ignore lint/complexity/noExcessiveCognitiveComplexity: RTC stats folding is intentionally comprehensive.
function foldInboundVideo(acc: VideoStats, report: RTCStatLike, stats: RTCStatsReport, prev: StatsPrev | null): void {
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
  acc.framesReceived = report.framesReceived ?? acc.framesReceived;
  acc.framesRendered = report.framesRendered ?? acc.framesRendered;
  acc.framesDropped = report.framesDropped ?? acc.framesDropped;
  acc.packetsDiscarded = report.packetsDiscarded ?? acc.packetsDiscarded;
  acc.jitterMs = report.jitter == null ? acc.jitterMs : report.jitter * 1000;
  if (report.jitterBufferEmittedCount && report.jitterBufferEmittedCount > 0) {
    acc.jitterBufferDelayMs = ((report.jitterBufferDelay ?? 0) / report.jitterBufferEmittedCount) * 1000;
    acc.jitterBufferTargetDelayMs = ((report.jitterBufferTargetDelay ?? 0) / report.jitterBufferEmittedCount) * 1000;
  }
}

const emptyVideoStats = (): VideoStats => ({
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
  framesReceived: 0,
  framesRendered: 0,
  framesDropped: 0,
  packetsDiscarded: 0,
  jitterMs: null,
  jitterBufferDelayMs: null,
  jitterBufferTargetDelayMs: null,
  decodeTimeMs: null,
  processingTimeMs: null,
  nackCount: 0,
  pliCount: 0,
  firCount: 0,
  retransmittedPacketsReceived: 0,
});

export function computeTelemetry(stats: RTCStatsReport, prev: StatsPrev | null, hasAudio: boolean): SpectatorTelemetry {
  const video = emptyVideoStats();
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
    framesReceived: video.framesReceived,
    framesRendered: video.framesRendered,
    framesDropped: video.framesDropped,
    packetsDiscarded: video.packetsDiscarded,
    jitterMs: video.jitterMs,
    jitterBufferDelayMs: video.jitterBufferDelayMs,
    jitterBufferTargetDelayMs: video.jitterBufferTargetDelayMs,
    decodeTimeMs: video.decodeTimeMs,
    processingTimeMs: video.processingTimeMs,
    nackCount: video.nackCount,
    pliCount: video.pliCount,
    firCount: video.firCount,
    retransmittedPacketsReceived: video.retransmittedPacketsReceived,
    decoderImplementation,
  };
}

// biome-ignore lint/complexity/noExcessiveCognitiveComplexity: snapshot extraction mirrors the stats schema.
export function createStatsPrev(stats: RTCStatsReport): StatsPrev | null {
  for (const reportRaw of stats.values()) {
    const report = reportRaw as RTCStatLike;
    if (report.type === 'inbound-rtp' && report.kind === 'video') {
      return {
        bytesReceived: report.bytesReceived ?? 0,
        packetsReceived: report.packetsReceived ?? 0,
        packetsLost: report.packetsLost ?? 0,
        framesReceived: report.framesReceived ?? 0,
        framesDecoded: report.framesDecoded ?? 0,
        framesRendered: report.framesRendered ?? 0,
        framesDropped: report.framesDropped ?? 0,
        packetsDiscarded: report.packetsDiscarded ?? 0,
        jitterBufferDelay: report.jitterBufferDelay ?? 0,
        jitterBufferTargetDelay: report.jitterBufferTargetDelay ?? 0,
        jitterBufferEmittedCount: report.jitterBufferEmittedCount ?? 0,
        totalDecodeTime: report.totalDecodeTime ?? 0,
        totalProcessingDelay: report.totalProcessingDelay ?? 0,
        nackCount: report.nackCount ?? 0,
        pliCount: report.pliCount ?? 0,
        firCount: report.firCount ?? 0,
        retransmittedPacketsReceived: report.retransmittedPacketsReceived ?? 0,
        ssrc: report.ssrc ?? 0,
        rid: report.rid ?? '',
        ts: report.timestamp ?? 0,
        init: true,
      };
    }
  }
  return null;
}
