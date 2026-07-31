import type React from 'react';

export const TelemetryCell: React.FC<{
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
