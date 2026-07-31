import { fmtBitrate, fmtLoss } from '@slopcast/shared-types';
import type React from 'react';
import type { SpectatorTelemetry } from './telemetry.js';

export { computeTelemetry, createStatsPrev } from './telemetry.js';
export type { SpectatorTelemetry };

const TelemetryCell: React.FC<{
  label: string;
  value: React.ReactNode;
  sub?: React.ReactNode;
  degrade?: boolean;
}> = ({ label, value, sub, degrade }) => (
  <div className="flex flex-col gap-0.5 min-w-0 shrink-0">
    <span
      className={`text-xs font-semibold uppercase tracking-wider leading-none ${
        degrade ? 'text-destructive/75' : 'text-muted-foreground'
      }`}
    >
      {label}
    </span>
    <span className="flex items-baseline gap-1.5 leading-none">
      <span
        className={`text-sm font-mono font-semibold tabular-nums leading-none ${
          degrade ? 'text-destructive' : 'text-foreground'
        }`}
      >
        {value}
      </span>
      {sub != null && <span className="text-xs font-mono tabular-nums leading-none text-caption-text">{sub}</span>}
    </span>
  </div>
);

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
