import { Monitor, X } from 'lucide-react';
import type React from 'react';
import { useEffect, useState } from 'react';

interface SpectatorBannerProps {
  compact?: boolean;
  autoFade?: boolean;
  fadeDelayMs?: number;
}

// Web clients are spectator-only by design: no capture APIs exist in this app,
// and this banner makes the restriction (and the desktop path) explicit.
export const SpectatorBanner: React.FC<SpectatorBannerProps> = ({ compact, autoFade = true, fadeDelayMs = 10000 }) => {
  const [isVisible, setIsVisible] = useState(true);

  useEffect(() => {
    if (!autoFade) return;

    const timer = setTimeout(() => {
      setIsVisible(false);
    }, fadeDelayMs);

    return () => clearTimeout(timer);
  }, [autoFade, fadeDelayMs]);

  const fadeClasses = `transition-opacity duration-1000 ease-out ${
    isVisible ? 'opacity-100' : 'opacity-0 pointer-events-none'
  }`;

  if (compact) {
    return (
      <div
        className={`flex items-start justify-between gap-2 text-[11px] leading-snug text-white/40 bg-black/50 border border-white/10 rounded-lg pl-2.5 pr-1.5 py-1.5 backdrop-blur-md max-w-[280px] pointer-events-auto ${fadeClasses}`}
      >
        <p className="flex-1">
          Web spectators can view screenshares. To host or share your screen, please open the Desktop App.
        </p>
        <button
          type="button"
          onClick={() => setIsVisible(false)}
          aria-label="Dismiss banner"
          title="Dismiss banner"
          className="p-0.5 text-white/40 hover:text-white transition-colors shrink-0 rounded focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-white/50"
        >
          <X className="w-3.5 h-3.5" />
        </button>
      </div>
    );
  }

  return (
    <div
      className={`w-full max-w-2xl mt-6 flex items-center justify-between gap-2.5 rounded-lg border border-border/60 bg-card/60 px-4 py-3 ${fadeClasses}`}
    >
      <div className="flex items-center gap-2.5 min-w-0">
        <Monitor className="w-4 h-4 shrink-0 text-muted-foreground" aria-hidden="true" />
        <p className="text-xs text-muted-foreground leading-relaxed text-center sm:text-left">
          Web spectators can view screenshares. To host or share your screen, please open the Desktop App.
        </p>
      </div>
      <button
        type="button"
        onClick={() => setIsVisible(false)}
        aria-label="Dismiss banner"
        title="Dismiss banner"
        className="p-1 text-muted-foreground hover:text-foreground transition-colors shrink-0 rounded-md focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        <X className="w-4 h-4" />
      </button>
    </div>
  );
};
