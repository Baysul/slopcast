import { fmtBitrate, fmtLoss } from '@slopcast/shared-types';
import type React from 'react';
import { Sparkline } from './Sparkline';
import { TelemetryCell } from './TelemetryCell';
import type { StreamTelemetry } from './types';
import { fmtDuration } from './types';

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
  let fpsValue = '\u2014';
  let fpsSub: string | undefined;
  if (t.frameRate != null) {
    fpsValue = `${Math.round(t.frameRate)} fps`;
  }
  if (t.targetFrameRate != null) {
    fpsSub = `/ ${Math.round(t.targetFrameRate)}`;
  }

  return (
    <div className="pointer-events-none absolute inset-x-0 bottom-0 z-20 select-none">
      <div className="absolute inset-0 bg-gradient-to-t from-black/95 via-black/75 to-transparent" />
      <div className="relative px-4 pt-12 pb-3">
        <div className="flex flex-wrap items-end gap-x-5 gap-y-3">
          <div className="flex items-center gap-1.5 shrink-0">
            <span className="w-2 h-2 rounded-full bg-safelight animate-pulse" />
            <span className="text-xs font-semibold uppercase tracking-wider text-safelight">On Air</span>
          </div>

          <TelemetryCell
            label="Codec"
            value={t.videoCodec ?? '\u2014'}
            sub={t.videoEncoder ? `\u00B7 ${t.videoEncoder}` : undefined}
          />
          <TelemetryCell label="Resolution" value={t.width && t.height ? `${t.width}\u00D7${t.height}` : '\u2014'} />
          <TelemetryCell label="Frame Rate" value={fpsValue} sub={fpsSub} degrade={fpsDegrade} />
          <TelemetryCell label="Bitrate" value={fmtBitrate(t.videoBitrate)} />
          <TelemetryCell label="Loss" value={fmtLoss(t.packetLossPct)} degrade={lossDegrade} />

          <div className="flex flex-col gap-0.5 shrink-0 min-w-0">
            <span className="text-xs font-semibold uppercase tracking-wider leading-none text-muted-foreground">
              Audio
            </span>
            <AudioTelemetryValue telemetry={t} />
          </div>

          <div className="grow basis-0 min-w-0" />

          <div className="flex items-end gap-3 shrink-0">
            <div className="flex flex-col items-end gap-1">
              <Sparkline data={t.bitrateHistory} />
              <span className="text-xs uppercase tracking-wider text-caption-text leading-none">
                {t.bitrateHistory.length > 0 ? `bitrate \u00B7 last ${t.bitrateHistory.length}s` : 'awaiting uplink'}
              </span>
            </div>
            <div className="flex flex-col gap-0.5 items-end">
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
