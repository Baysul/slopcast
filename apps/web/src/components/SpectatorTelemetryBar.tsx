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
        value={
          <span data-testid="spectator-telemetry-fps">
            {telemetry.frameRate != null ? `${Math.round(telemetry.frameRate)}` : '\u2014'}
          </span>
        }
        sub={
          telemetry.videoCodec ? <span data-testid="spectator-telemetry-codec">{telemetry.videoCodec}</span> : undefined
        }
        degrade={telemetry.quality === 'poor'}
      />

      <div className="hidden md:block">
        <TelemetryCell
          label="Jitter"
          value={telemetry.jitterMs != null ? `${telemetry.jitterMs.toFixed(1)} ms` : '\u2014'}
          sub={
            telemetry.jitterBufferDelayMs != null ? `buffer ${telemetry.jitterBufferDelayMs.toFixed(0)} ms` : undefined
          }
          degrade={telemetry.jitterMs != null && telemetry.jitterMs > 30}
        />
      </div>

      <div className="hidden lg:block">
        <TelemetryCell
          label="Frame drops"
          value={`${telemetry.framesDropped}`}
          sub={telemetry.packetsDiscarded > 0 ? `discarded total ${telemetry.packetsDiscarded}` : undefined}
          degrade={telemetry.framesDropped > 0}
        />
      </div>

      <div className="hidden xl:block">
        <TelemetryCell
          label="Freeze / NACK"
          value={`${telemetry.freezeCount}`}
          sub={`NACK ${telemetry.nackCount}`}
          degrade={telemetry.freezeCount > 0 || telemetry.nackCount > 0}
        />
      </div>

      <div className="hidden sm:block">
        <TelemetryCell label="Loss" value={fmtLoss(telemetry.packetLossPct)} degrade={lossDegrade} />
      </div>
    </div>
  );
};
