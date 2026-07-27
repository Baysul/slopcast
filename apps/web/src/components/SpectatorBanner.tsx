import { Monitor } from 'lucide-react';
import type React from 'react';

// Web clients are spectator-only by design: no capture APIs exist in this app,
// and this banner makes the restriction (and the desktop path) explicit.
export const SpectatorBanner: React.FC<{ compact?: boolean }> = ({ compact }) => {
  if (compact) {
    return (
      <p className="text-[11px] leading-snug text-white/40 bg-black/40 border border-white/10 rounded-lg px-2.5 py-1.5 backdrop-blur-md max-w-[260px]">
        Web spectators can view screenshares. To host or share your screen, please open the Desktop App.
      </p>
    );
  }
  return (
    <div className="w-full max-w-2xl mt-6 flex items-center justify-center gap-2.5 rounded-xl border border-border/60 bg-card/60 px-4 py-3">
      <Monitor className="w-4 h-4 shrink-0 text-muted-foreground" aria-hidden="true" />
      <p className="text-xs text-muted-foreground leading-relaxed text-center">
        Web spectators can view screenshares. To host or share your screen, please open the Desktop App.
      </p>
    </div>
  );
};
