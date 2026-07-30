import { codecLabel, fmtBitrate, fmtLoss } from '@slopcast/shared-types';
import type React from 'react';

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

const TelemetryCell: React.FC<{
  label: string;
  value: React.ReactNode;
  sub?: React.ReactNode;
  degrade?: boolean;
}> = ({ label, value, sub, degrade }) => (
  <div className="flex flex-col gap-0.5 min-w-0 shrink-0">
    <span
      className={`text-[9px] font-semibold uppercase tracking-[0.1em] leading-none ${
        degrade ? 'text-destructive/75' : 'text-gray-500'
      }`}
    >
      {label}
    </span>
    <span className="flex items-baseline gap-1.5 leading-none">
      <span
        className={`text-sm font-mono font-semibold tabular-nums leading-none ${
          degrade ? 'text-destructive' : 'text-gray-100'
        }`}
      >
        {value}
      </span>
      {sub != null && <span className="text-[10px] font-mono tabular-nums leading-none text-gray-600">{sub}</span>}
    </span>
  </div>
);

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

export const SpectatorTelemetryBar: React.FC<{ telemetry: SpectatorTelemetry }> = ({ telemetry }) => {
  const lossDegrade = telemetry.packetLossPct != null && telemetry.packetLossPct > 1;

  return (
    <div className="flex items-end gap-x-5 gap-y-3 flex-wrap">
      <TelemetryCell label="Bitrate" value={fmtBitrate(telemetry.videoBitrate)} />

      <div className="hidden sm:block">
        <TelemetryCell
          label="Resolution"
          value={telemetry.width && telemetry.height ? `${telemetry.width}\u00d7${telemetry.height}` : '\u2014'}
        />
      </div>

      <TelemetryCell
        label="FPS"
        value={telemetry.frameRate != null ? `${Math.round(telemetry.frameRate)}` : '\u2014'}
        sub={telemetry.videoCodec ?? undefined}
        degrade={telemetry.quality === 'poor'}
      />

      <div className="hidden sm:block">
        <TelemetryCell label="Loss" value={fmtLoss(telemetry.packetLossPct)} degrade={lossDegrade} />
      </div>
    </div>
  );
};
