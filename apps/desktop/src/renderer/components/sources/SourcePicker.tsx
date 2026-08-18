import { Check, Copy, Video, X } from 'lucide-react';
import { motion } from 'motion/react';
import * as React from 'react';
import { useEffect, useState } from 'react';

import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import type { CaptureContext, CaptureSourceSelection, CaptureStage } from '../../types';
import { CaptureSourcePicker } from './CaptureSourcePicker';

function usePrefersReducedMotion(): boolean {
  const [prefersReduced, setPrefersReduced] = useState(false);

  useEffect(() => {
    const mediaQuery = window.matchMedia('(prefers-reduced-motion: reduce)');
    setPrefersReduced(mediaQuery.matches);

    const handleChange = (): void => setPrefersReduced(mediaQuery.matches);
    mediaQuery.addEventListener('change', handleChange);
    return () => mediaQuery.removeEventListener('change', handleChange);
  }, []);

  return prefersReduced;
}

export interface SourcePickerProps {
  roomCode: string;
  isCreatingRoom: boolean;
  copied: 'link' | 'code' | null;
  captureContext: CaptureContext | null;
  autoDetectFailed: boolean;
  captureStage: CaptureStage;
  showStopConfirm: boolean;
  setShowStopConfirm: (show: boolean) => void;
  spectatorCount: number;
  canStartShare: boolean;
  canGoLive: boolean;
  disabledReason: string | null;
  /// Windows-only: the in-app WGC source picker is embedded in this card
  /// while open (there is no Windows system picker).
  pickerOpen: boolean;
  setPickerOpen: (open: boolean) => void;
  onSourceSelected: (selection: CaptureSourceSelection) => void;
  onCreateRoom: () => void;
  onCopyCode: () => void;
  onCopyLink: () => void;
  onStartShare: () => void;
  onGoLive: () => void;
  onStopShare: () => void;
}

interface RoomControlsProps {
  roomCode: string;
  isCreatingRoom: boolean;
  copied: 'link' | 'code' | null;
  spectatorCount: number;
  onCreateRoom: () => void;
  onCopyCode: () => void;
  onCopyLink: () => void;
}

// Room lifecycle controls that used to live in the presenter header: create
// the room when none exists, otherwise show the share code and copy actions.
const RoomControls: React.FC<RoomControlsProps> = React.memo(
  ({ roomCode, isCreatingRoom, copied, spectatorCount, onCreateRoom, onCopyCode, onCopyLink }) => {
    if (!roomCode) {
      return (
        <Button
          variant="default"
          onClick={onCreateRoom}
          disabled={isCreatingRoom}
          aria-busy={isCreatingRoom}
          className="w-full font-bold"
        >
          {isCreatingRoom ? 'Creating Room…' : 'Create Live Room'}
        </Button>
      );
    }

    return (
      <div className="space-y-3" aria-live="polite">
        <div className="flex items-center gap-2 flex-wrap">
          {spectatorCount > 0 && (
            <Badge variant="info" className="tabular-nums">
              {spectatorCount} spectator{spectatorCount === 1 ? '' : 's'}
            </Badge>
          )}
          <span className="font-mono text-sm font-semibold tabular-nums tracking-wide text-foreground/90">
            {roomCode}
          </span>
          <Button variant="secondary" size="sm" onClick={onCopyCode} className="gap-1.5">
            {copied === 'code' ? <Check className="w-3.5 h-3.5 text-safelight" aria-hidden="true" /> : null}
            {copied === 'code' ? 'Copied' : 'Copy code'}
          </Button>
          <Button size="sm" onClick={onCopyLink} className="gap-1.5">
            {copied === 'link' ? (
              <Check className="w-4 h-4" aria-hidden="true" />
            ) : (
              <Copy className="w-4 h-4" aria-hidden="true" />
            )}
            {copied === 'link' ? 'Link copied' : 'Copy link'}
          </Button>
        </div>
      </div>
    );
  },
);

RoomControls.displayName = 'RoomControls';

export const SourcePicker: React.FC<SourcePickerProps> = React.memo(
  ({
    roomCode,
    isCreatingRoom,
    copied,
    captureContext,
    autoDetectFailed,
    captureStage,
    showStopConfirm,
    setShowStopConfirm,
    spectatorCount,
    canStartShare,
    canGoLive,
    disabledReason,
    pickerOpen,
    setPickerOpen,
    onSourceSelected,
    onCreateRoom,
    onCopyCode,
    onCopyLink,
    onStartShare,
    onGoLive,
    onStopShare,
    // biome-ignore lint/complexity/noExcessiveCognitiveComplexity: presenter source surface owns room, capture stages, KDE notices and stop-confirm; splitting would trade clarity for indirection
  }) => {
    const shouldReduceMotion = usePrefersReducedMotion();
    const [kdeNoticeDismissed, setKdeNoticeDismissed] = useState(false);
    const [kdeFailedNoticeDismissed, setKdeFailedNoticeDismissed] = useState(false);

    return (
      <Card>
        <CardHeader>
          <CardTitle className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
            Screenshare Source
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          <RoomControls
            roomCode={roomCode}
            isCreatingRoom={isCreatingRoom}
            copied={copied}
            spectatorCount={spectatorCount}
            onCreateRoom={onCreateRoom}
            onCopyCode={onCopyCode}
            onCopyLink={onCopyLink}
          />

          {captureContext?.de === 'kde' && !autoDetectFailed && !kdeNoticeDismissed && (
            <div className="relative bg-secondary border border-border rounded-lg p-3">
              <p className="text-sm text-muted-foreground leading-relaxed pr-6">
                KDE Plasma detected — window identity is unavailable in PipeWire streams. If auto-detection fails,
                select an audio app manually.
              </p>
              <button
                type="button"
                onClick={() => setKdeNoticeDismissed(true)}
                className="absolute top-2 right-2 p-1 rounded-md text-muted-foreground hover:text-foreground hover:bg-accent/50 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70"
                aria-label="Dismiss KDE Plasma notice"
              >
                <X className="w-3.5 h-3.5" aria-hidden="true" />
              </button>
            </div>
          )}

          {autoDetectFailed && captureContext?.de === 'kde' && !kdeFailedNoticeDismissed && (
            <div className="relative bg-secondary border border-border rounded-lg p-3 space-y-1">
              <p className="text-xs font-semibold text-foreground pr-6">KDE Audio Auto-Detection Failed</p>
              <p className="text-sm text-muted-foreground leading-relaxed pr-6">
                Select an audio app from the panel above, then stop and restart the screenshare.
              </p>
              <button
                type="button"
                onClick={() => setKdeFailedNoticeDismissed(true)}
                className="absolute top-2 right-2 p-1 rounded-md text-muted-foreground hover:text-foreground hover:bg-accent/50 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70"
                aria-label="Dismiss KDE auto-detection failure notice"
              >
                <X className="w-3.5 h-3.5" aria-hidden="true" />
              </button>
            </div>
          )}

          {pickerOpen && <CaptureSourcePicker onSelect={onSourceSelected} onCancel={() => setPickerOpen(false)} />}

          {captureStage === 'idle' && (
            <div className="space-y-2.5">
              <Button
                variant="default"
                onClick={onStartShare}
                disabled={!canStartShare}
                aria-describedby={disabledReason ? 'start-screenshare-hint' : 'start-screenshare-ready-hint'}
                className="group relative w-full font-bold overflow-hidden shadow-[inset_0_1px_0_rgba(255,255,255,0.14)] hover:shadow-[inset_0_1px_0_rgba(255,255,255,0.18)] active:shadow-[inset_0_1px_1px_rgba(0,0,0,0.2)] active:scale-[0.99] transition-[transform,box-shadow,background-color] duration-200 ease-out disabled:shadow-none disabled:active:scale-100"
              >
                {/* Idle shimmer — motion.dev loop, just enough to draw the eye. Hidden when disabled or reduced-motion. */}
                {canStartShare && !shouldReduceMotion && (
                  <motion.span
                    aria-hidden="true"
                    className="pointer-events-none absolute inset-0 bg-gradient-to-r from-transparent via-white/[0.14] to-transparent"
                    initial={{ x: '-100%' }}
                    animate={{ x: '100%' }}
                    transition={{
                      duration: 0.95,
                      ease: [0.16, 1, 0.3, 1],
                      repeat: Infinity,
                      repeatDelay: 3.2,
                      repeatType: 'loop',
                    }}
                    style={{ willChange: 'transform' }}
                  />
                )}
                {/* Hover wash — quick darkroom sweep on interaction. */}
                <span
                  aria-hidden="true"
                  className={`pointer-events-none absolute inset-0 -translate-x-full bg-gradient-to-r from-transparent via-white/[0.1] to-transparent opacity-0 transition-[transform,opacity] duration-[520ms] ease-[cubic-bezier(0.16,1,0.3,1)] group-hover:translate-x-full group-hover:opacity-100 group-focus-visible:translate-x-full group-focus-visible:opacity-100 motion-reduce:hidden ${!canStartShare ? 'hidden' : ''}`}
                />
                {/* Viewfinder brackets — capture frame at the edges. */}
                <span
                  aria-hidden="true"
                  className={`pointer-events-none absolute inset-[5px] rounded-[7px] border transition-colors duration-200 ${!canStartShare ? 'border-transparent' : 'border-transparent group-hover:border-white/10 group-focus-visible:border-white/10'}`}
                />
                <span className="relative flex items-center justify-center gap-2.5">
                  <Video
                    aria-hidden="true"
                    className={`h-4 w-4 shrink-0 transition-[transform,opacity] duration-200 ease-out motion-reduce:transition-none ${!canStartShare ? 'opacity-60' : 'group-hover:scale-[1.08] group-focus-visible:scale-[1.08] group-active:scale-95'}`}
                  />
                  Start Screenshare
                </span>
              </Button>
              {disabledReason ? (
                <p id="start-screenshare-hint" className="text-sm leading-relaxed text-muted-foreground">
                  {disabledReason}
                </p>
              ) : (
                <p id="start-screenshare-ready-hint" className="text-center text-xs leading-relaxed text-caption-text">
                  Preview first — Go Live when the frame is ready
                </p>
              )}
            </div>
          )}

          {captureStage === 'previewing' && (
            <div className="space-y-2">
              <div className="flex gap-2">
                <Button variant="default" onClick={onGoLive} disabled={!canGoLive} className="flex-1 font-bold">
                  Go Live
                </Button>
                <Button variant="secondary" onClick={onStopShare} className="flex-1">
                  Cancel
                </Button>
              </div>
              <p className="text-sm text-muted-foreground leading-relaxed">
                {canGoLive
                  ? 'Previewing your capture — click Go Live to start broadcasting.'
                  : 'Choose what to share in the portal dialog to preview it, then go live.'}
              </p>
            </div>
          )}

          {captureStage === 'live' && (
            <div className="space-y-2">
              {!showStopConfirm ? (
                <Button variant="destructive" onClick={() => setShowStopConfirm(true)} className="w-full font-bold">
                  Stop Screenshare
                </Button>
              ) : (
                <div className="space-y-2">
                  <p className="text-sm text-muted-foreground text-center">
                    {spectatorCount > 0
                      ? `${spectatorCount} spectator${spectatorCount === 1 ? '' : 's'} watching. Stop streaming?`
                      : 'Stop the stream?'}
                  </p>
                  <div className="flex gap-2">
                    <Button
                      variant="destructive"
                      onClick={() => {
                        setShowStopConfirm(false);
                        onStopShare();
                      }}
                      className="flex-1 font-bold"
                    >
                      Stop
                    </Button>
                    <Button variant="secondary" onClick={() => setShowStopConfirm(false)} className="flex-1">
                      Cancel
                    </Button>
                  </div>
                </div>
              )}
            </div>
          )}
        </CardContent>
      </Card>
    );
  },
);

SourcePicker.displayName = 'SourcePicker';
