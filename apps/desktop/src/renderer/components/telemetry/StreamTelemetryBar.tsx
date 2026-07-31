import { fmtBitrate, fmtLoss } from '@slopcast/shared-types';
import { TriangleAlert } from 'lucide-react';
import type React from 'react';

export interface StreamTelemetry {
  live: boolean;
  videoCodec: string | null;
  videoEncoder: string | null;
  audioCodec: string | null;
  width: number | null;
  height: number | null;
  frameRate: number | null;
  targetFrameRate: number | null;
  videoBitrate: number | null;
  audioBitrate: number | null;
  packetLossPct: number | null;
  rttMs: number | null;
  spectatorCount: number;
  bitrateHistory: number[];
  elapsedMs: number;
  hasAudio: boolean;
}

export function idleTelemetry(): StreamTelemetry {
  return {
    live: false,
    videoCodec: null,
    videoEncoder: null,
    audioCodec: null,
    width: null,
    height: null,
    frameRate: null,
    targetFrameRate: null,
    videoBitrate: null,
    audioBitrate: null,
    packetLossPct: null,
    rttMs: null,
    spectatorCount: 0,
    bitrateHistory: [],
    elapsedMs: 0,
    hasAudio: false,
  };
}

export function fmtDuration(ms: number): string {
  const totalSec = Math.floor(ms / 1000);
  const m = Math.floor(totalSec / 60);
  const s = totalSec % 60;
  return `${m.toString().padStart(2, '0')}:${s.toString().padStart(2, '0')}`;
}

const Sparkline: React.FC<{ data: number[]; width?: number; height?: number }> = ({
  data,
  width = 88,
  height = 22,
}) => {
  let points = '';
  if (data.length >= 2) {
    const max = Math.max(...data);
    const min = Math.min(...data, 0);
    const span = Math.max(max - min, 0.001);
    const step = width / (data.length - 1);
    points = data
      .map((v, i) => {
        const x = i * step;
        const y = height - ((v - min) / span) * (height - 3) - 1.5;
        return `${x.toFixed(1)},${y.toFixed(1)}`;
      })
      .join(' ');
  }
  return (
    <svg
      width={width}
      height={height}
      viewBox={`0 0 ${width} ${height}`}
      className="block text-safelight"
      aria-hidden="true"
      preserveAspectRatio="none"
    >
      {points && (
        <polyline
          points={points}
          fill="none"
          stroke="currentColor"
          strokeWidth={1.5}
          strokeLinejoin="round"
          strokeLinecap="round"
          vectorEffect="non-scaling-stroke"
        />
      )}
    </svg>
  );
};

const TelemetryCell: React.FC<{
  label: string;
  value: React.ReactNode;
  sub?: React.ReactNode;
  degrade?: boolean;
}> = ({ label, value, sub, degrade }) => {
  const labelClass = degrade ? 'text-destructive/75' : 'text-muted-foreground';
  const valueClass = degrade ? 'text-destructive' : 'text-foreground';

  return (
    <div className="flex flex-col gap-1 min-w-0 shrink-0">
      <span className={`text-xs font-semibold uppercase tracking-wider leading-none ${labelClass}`}>{label}</span>
      <span className="flex items-baseline gap-1.5 leading-none">
        <span className={`text-sm font-mono font-semibold tabular-nums leading-none ${valueClass}`}>
          {degrade && <TriangleAlert className="inline size-3 mr-1 -mt-0.5" aria-hidden="true" />}
          {value}
        </span>
        {sub != null && <span className="text-xs font-mono tabular-nums leading-none text-caption-text">{sub}</span>}
      </span>
    </div>
  );
};

const AudioTelemetryValue: React.FC<{ telemetry: StreamTelemetry }> = ({ telemetry: t }) => {
  if (!t.hasAudio) {
    return <span className="text-sm font-mono font-semibold leading-none text-caption-text">video only</span>;
  }
  return (
    <span className="flex items-baseline gap-1.5 leading-none">
      <span className="text-sm font-mono font-semibold tabular-nums leading-none text-foreground">
        {t.audioCodec ?? '\u2014'}
      </span>
      {t.audioBitrate != null && (
        <span className="text-xs font-mono tabular-nums leading-none text-muted-foreground">
          {fmtBitrate(t.audioBitrate)}
        </span>
      )}
    </span>
  );
};

export const StreamTelemetryBar: React.FC<{ telemetry: StreamTelemetry }> = ({ telemetry: t }) => {
  const fpsDegrade = t.frameRate != null && t.targetFrameRate != null && t.frameRate < t.targetFrameRate * 0.75;
  const lossDegrade = t.packetLossPct != null && t.packetLossPct > 1;
  let fpsValue = '—';
  let fpsSub: string | undefined;
  if (t.frameRate != null) {
    fpsValue = `${Math.round(t.frameRate)} fps`;
  }
  if (t.targetFrameRate != null) {
    fpsSub = `/ ${Math.round(t.targetFrameRate)}`;
  }

  const sparklineSub = t.bitrateHistory.length > 0 ? `bitrate · last ${t.bitrateHistory.length}s` : 'awaiting uplink';

  return (
    <div className="pointer-events-none absolute inset-x-0 bottom-0 z-20 select-none">
      <div className="absolute inset-0 bg-gradient-to-t from-black/95 via-black/75 to-transparent" />
      <div className="relative px-4 pt-12 pb-3">
        <div className="flex flex-wrap items-end gap-x-5 gap-y-3">
          <div className="flex items-center gap-1.5 shrink-0">
            <span className="w-2 h-2 rounded-full bg-safelight motion-safe:animate-pulse" />
            <span className="text-xs font-semibold uppercase tracking-wider text-safelight">On Air</span>
          </div>

          <TelemetryCell
            label="Codec"
            value={t.videoCodec ?? '—'}
            sub={t.videoEncoder ? `· ${t.videoEncoder}` : undefined}
          />
          <TelemetryCell label="Resolution" value={t.width && t.height ? `${t.width}×${t.height}` : '—'} />
          <TelemetryCell label="Frame Rate" value={fpsValue} sub={fpsSub} degrade={fpsDegrade} />
          <TelemetryCell label="Bitrate" value={fmtBitrate(t.videoBitrate)} />
          <TelemetryCell label="Loss" value={fmtLoss(t.packetLossPct)} degrade={lossDegrade} />

          <div className="flex flex-col gap-1 shrink-0 min-w-0">
            <span className="text-xs font-semibold uppercase tracking-wider leading-none text-muted-foreground">
              Audio
            </span>
            <AudioTelemetryValue telemetry={t} />
          </div>

          <div className="grow basis-0 min-w-0" />

          <div className="flex items-end gap-3 shrink-0">
            <div className="flex flex-col items-end gap-1">
              <Sparkline data={t.bitrateHistory} />
              <span className="text-xs uppercase tracking-wider text-caption-text leading-none">{sparklineSub}</span>
            </div>
            <div className="flex flex-col gap-1 items-end">
              <span className="text-xs font-semibold uppercase tracking-wider leading-none text-muted-foreground">
                Elapsed
              </span>
              <span className="text-xs font-mono font-semibold tabular-nums leading-none text-foreground">
                {fmtDuration(t.elapsedMs)}
              </span>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
};
