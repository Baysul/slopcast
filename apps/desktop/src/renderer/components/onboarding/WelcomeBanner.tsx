import { ArrowRight, ScreenShare, Users, X } from 'lucide-react';
import type React from 'react';
import { useCallback, useEffect, useState } from 'react';
import { useOnboarding } from '@/hooks/useOnboarding';

export const WelcomeBanner: React.FC = () => {
  const { completed, dismiss } = useOnboarding();
  const [visible, setVisible] = useState(false);
  const [exiting, setExiting] = useState(false);

  useEffect(() => {
    if (!completed) {
      const timer = setTimeout(() => setVisible(true), 400);
      return () => clearTimeout(timer);
    }
    return undefined;
  }, [completed]);

  const handleDismiss = useCallback(() => {
    setExiting(true);
  }, []);

  const handleAnimationEnd = useCallback(() => {
    if (exiting) {
      dismiss();
    }
  }, [exiting, dismiss]);

  if (completed && !exiting) return null;

  return (
    <div
      onAnimationEnd={handleAnimationEnd}
      className={`relative overflow-hidden transition-all duration-500 ease-out ${
        exiting ? 'max-h-0 opacity-0 mb-0' : 'max-h-[500px] opacity-100 mb-8'
      } ${!visible && !exiting ? 'max-h-0 opacity-0 mb-0' : ''}`}
      aria-hidden={exiting || !visible}
    >
      <div className="rounded-xl border border-border bg-gradient-to-br from-[#111827]/95 via-[#111827]/90 to-[#0f172a]/95 backdrop-blur-md overflow-hidden">
        <div className="absolute top-0 right-0 w-64 h-64 bg-safelight/3 rounded-full blur-3xl -translate-y-1/2 translate-x-1/4 pointer-events-none" />

        <button
          type="button"
          onClick={handleDismiss}
          className="absolute top-3 right-3 p-1.5 rounded-md text-muted-foreground hover:text-foreground hover:bg-secondary transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70"
          aria-label="Dismiss welcome guide"
        >
          <X className="size-4" aria-hidden="true" />
        </button>

        <div className="p-6 pb-7">
          <div className="flex items-center gap-3 mb-4">
            <span className="p-2 bg-safelight/10 rounded-xl shrink-0">
              <ScreenShare className="size-5 text-safelight" aria-hidden="true" />
            </span>
            <div>
              <h2 className="text-base font-bold text-foreground">Welcome to Slopcast</h2>
              <p className="text-xs text-muted-foreground">Share your screen with surgical audio control</p>
            </div>
          </div>

          <div className="grid grid-cols-1 sm:grid-cols-3 gap-3">
            <div className="flex gap-3 p-3 rounded-lg bg-secondary border border-border">
              <span className="flex-shrink-0 w-6 h-6 rounded-full bg-safelight/15 text-safelight text-xs font-bold flex items-center justify-center">
                <ScreenShare className="size-3.5" aria-hidden="true" />
              </span>
              <div>
                <p className="text-sm font-semibold text-foreground">Create a room</p>
                <p className="text-xs text-muted-foreground mt-0.5 leading-relaxed">
                  Click <span className="text-foreground font-medium">Create Live Room</span> in the header to get a
                  shareable link.
                </p>
              </div>
            </div>

            <div className="flex gap-3 p-3 rounded-lg bg-secondary border border-border">
              <span className="flex-shrink-0 w-6 h-6 rounded-full bg-safelight/15 text-safelight text-xs font-bold flex items-center justify-center">
                <Users className="size-3.5" aria-hidden="true" />
              </span>
              <div>
                <p className="text-sm font-semibold text-foreground">Pick what to share</p>
                <p className="text-xs text-muted-foreground mt-0.5 leading-relaxed">
                  Select a window or video file. Audio is auto-detected from your chosen window.
                </p>
              </div>
            </div>

            <div className="flex gap-3 p-3 rounded-lg bg-secondary border border-border">
              <span className="flex-shrink-0 w-6 h-6 rounded-full bg-safelight/15 text-safelight text-xs font-bold flex items-center justify-center">
                <ArrowRight className="size-3.5" aria-hidden="true" />
              </span>
              <div>
                <p className="text-sm font-semibold text-foreground">Go live</p>
                <p className="text-xs text-muted-foreground mt-0.5 leading-relaxed">
                  Hit <span className="text-foreground font-medium">Start Screenshare</span>. Share the link —
                  spectators join instantly, no install.
                </p>
              </div>
            </div>
          </div>

          <div className="mt-5 flex items-center justify-between">
            <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
              <Users className="size-3.5" aria-hidden="true" />
              <span>Spectators watch in-browser — no account needed</span>
            </div>
            <button
              type="button"
              onClick={handleDismiss}
              className="flex items-center gap-1.5 text-xs font-medium text-safelight hover:text-safelight-hover transition-colors focus:outline-none focus-visible:underline"
            >
              Got it
              <ArrowRight className="size-3.5" aria-hidden="true" />
            </button>
          </div>
        </div>
      </div>
    </div>
  );
};
