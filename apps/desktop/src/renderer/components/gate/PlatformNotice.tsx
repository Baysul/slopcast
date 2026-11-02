import { MonitorX } from 'lucide-react';
import React from 'react';

interface PlatformNoticeProps {
  readonly platform: string;
}

export const PlatformNotice: React.FC<PlatformNoticeProps> = React.memo(({ platform }: PlatformNoticeProps) => {
  const waylandRequired = platform === 'linux';
  let title = 'Screen capture not supported';
  let description = 'Screen sharing is not available on this platform yet.';
  let remedy = 'Run Slopcast on Linux (Wayland) or Windows to share your screen.';
  if (waylandRequired) {
    title = 'Wayland session required';
    description = 'Slopcast requires a Wayland session (KDE/GNOME). Screen sharing is not available on X11.';
    remedy = 'Log out and choose a Wayland session in your display manager, then start Slopcast again.';
  }
  return (
    <div className="h-full flex items-center justify-center px-6">
      <div className="w-full max-w-lg rounded-lg border border-border bg-gradient-to-br from-secondary/80 via-secondary/80 to-background/95 backdrop-blur-md overflow-hidden">
        <div className="absolute top-0 right-0 w-64 h-64 bg-safelight/5 rounded-full blur-3xl -translate-y-1/2 translate-x-1/4 pointer-events-none" />
        <div className="p-8 text-center">
          <span className="inline-flex p-3 bg-safelight/10 rounded-xl mb-4">
            <MonitorX className="size-6 text-safelight" aria-hidden="true" />
          </span>
          <h1 className="text-lg font-bold text-foreground">{title}</h1>
          <p className="text-sm text-muted-foreground mt-2 leading-relaxed">{description}</p>
          <p className="text-xs text-caption-text mt-4 leading-relaxed">{remedy}</p>
        </div>
      </div>
    </div>
  );
});

PlatformNotice.displayName = 'PlatformNotice';
