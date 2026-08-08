import { MonitorX } from 'lucide-react';
import React from 'react';

// Full-height gate rendered when the backend reports no video capture route
// (gated behind the platform video check): no share controls render at all, since the backend is
// in `unsupported` state and every capture command errors. On Linux that
// means a non-Wayland session (X11); on other platforms (macOS) screen
// capture is not implemented yet. Windows never renders this — WGC is a
// capture route. Styling reuses the WelcomeBanner card treatment; `h-full`
// (not `min-h-screen`) because it renders inside the app shell below the
// titlebar.
export const PlatformNotice: React.FC<{ platform: string }> = React.memo(({ platform }) => {
  const waylandRequired = platform === 'linux';
  return (
    <div className="h-full flex items-center justify-center px-6">
      <div className="w-full max-w-lg rounded-lg border border-border bg-gradient-to-br from-secondary/80 via-secondary/80 to-background/95 backdrop-blur-md overflow-hidden">
        <div className="absolute top-0 right-0 w-64 h-64 bg-safelight/5 rounded-full blur-3xl -translate-y-1/2 translate-x-1/4 pointer-events-none" />
        <div className="p-8 text-center">
          <span className="inline-flex p-3 bg-safelight/10 rounded-xl mb-4">
            <MonitorX className="size-6 text-safelight" aria-hidden="true" />
          </span>
          <h1 className="text-lg font-bold text-foreground">
            {waylandRequired ? 'Wayland session required' : 'Screen capture not supported'}
          </h1>
          <p className="text-sm text-muted-foreground mt-2 leading-relaxed">
            {waylandRequired
              ? 'Slopcast requires a Wayland session (KDE/GNOME). Screen sharing is not available on X11.'
              : 'Screen sharing is not available on this platform yet.'}
          </p>
          <p className="text-xs text-caption-text mt-4 leading-relaxed">
            {waylandRequired
              ? 'Log out and choose a Wayland session in your display manager, then start Slopcast again.'
              : 'Run Slopcast on Linux (Wayland) or Windows to share your screen.'}
          </p>
        </div>
      </div>
    </div>
  );
});

PlatformNotice.displayName = 'PlatformNotice';
