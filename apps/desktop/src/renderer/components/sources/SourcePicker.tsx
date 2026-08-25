import { Check, Copy, Link2Off, Video, X } from 'lucide-react';
import { motion } from 'motion/react';
import * as React from 'react';
import { useEffect, useState } from 'react';

import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from '@/components/ui/alert-dialog';
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
  isClosingRoom: boolean;
  isRoomTransitioning: boolean;
  canCreateRoom: boolean;
  roomCreateDisabledReason: string | null;
  hasRetryableRoom: boolean;
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
  pickerOpen: boolean;
  setPickerOpen: (open: boolean) => void;
  onSourceSelected: (selection: CaptureSourceSelection) => void;
  onCreateRoom: () => void;
  onRetryRoomConnection: () => void;
  onCloseRoom: () => void;
  onCopyCode: () => void;
  onCopyLink: () => void;
  onStartShare: () => void;
  onGoLive: () => void;
  onStopShare: () => void;
}

interface RoomControlsProps {
  roomCode: string;
  isCreatingRoom: boolean;
  isClosingRoom: boolean;
  canCreateRoom: boolean;
  roomCreateDisabledReason: string | null;
  hasRetryableRoom: boolean;
  copied: 'link' | 'code' | null;
  spectatorCount: number;
  onCreateRoom: () => void;
  onRetryRoomConnection: () => void;
  onCloseRoom: () => void;
  onCopyCode: () => void;
  onCopyLink: () => void;
}

type EmptyRoomControlsProps = Pick<
  RoomControlsProps,
  | 'canCreateRoom'
  | 'hasRetryableRoom'
  | 'isCreatingRoom'
  | 'onCreateRoom'
  | 'onRetryRoomConnection'
  | 'roomCreateDisabledReason'
>;

function EmptyRoomControls({
  canCreateRoom,
  hasRetryableRoom,
  isCreatingRoom,
  onCreateRoom,
  onRetryRoomConnection,
  roomCreateDisabledReason,
}: EmptyRoomControlsProps): React.ReactNode {
  let buttonLabel = 'Create Live Room';
  if (isCreatingRoom) buttonLabel = 'Connecting Room…';
  else if (hasRetryableRoom) buttonLabel = 'Retry connection';

  return (
    <div className="space-y-2">
      <Button
        variant="default"
        onClick={hasRetryableRoom ? onRetryRoomConnection : onCreateRoom}
        disabled={isCreatingRoom || (!hasRetryableRoom && !canCreateRoom)}
        aria-busy={isCreatingRoom}
        aria-describedby={roomCreateDisabledReason ? 'create-room-hint' : undefined}
        className="w-full font-bold"
      >
        {buttonLabel}
      </Button>
      {roomCreateDisabledReason && !hasRetryableRoom && (
        <p id="create-room-hint" className="text-sm leading-relaxed text-muted-foreground">
          {roomCreateDisabledReason}
        </p>
      )}
      {hasRetryableRoom && (
        <p className="text-sm leading-relaxed text-muted-foreground">
          The replacement room is ready, but LiveKit did not connect. Retry within 45 seconds.
        </p>
      )}
    </div>
  );
}

type ActiveRoomControlsProps = Pick<
  RoomControlsProps,
  'copied' | 'isClosingRoom' | 'onCloseRoom' | 'onCopyCode' | 'onCopyLink' | 'roomCode' | 'spectatorCount'
>;

function ActiveRoomControls({
  copied,
  isClosingRoom,
  onCloseRoom,
  onCopyCode,
  onCopyLink,
  roomCode,
  spectatorCount,
}: ActiveRoomControlsProps): React.ReactNode {
  const spectatorLabel = `${spectatorCount} spectator${spectatorCount === 1 ? '' : 's'}`;
  let closeDescription = 'Sharing will stop and this room link will stop working. This cannot be undone.';
  if (spectatorCount > 0) {
    closeDescription = `Sharing will stop, ${spectatorLabel} will disconnect, and this room link will stop working. This cannot be undone.`;
  }

  return (
    <div className="space-y-3" aria-live="polite">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-mono text-sm font-semibold tabular-nums tracking-wide text-foreground/90">
          {roomCode}
        </span>
        {spectatorCount > 0 && (
          <Badge variant="info" className="tabular-nums">
            {spectatorLabel}
          </Badge>
        )}
      </div>
      <div className="grid grid-cols-3 gap-2">
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
        <AlertDialog>
          <AlertDialogTrigger asChild>
            <Button
              variant="outline"
              size="sm"
              disabled={isClosingRoom}
              className="gap-1.5 border-destructive/50 text-destructive hover:bg-destructive/10 hover:text-destructive"
            >
              <Link2Off className="size-4" aria-hidden="true" />
              {isClosingRoom ? 'Closing…' : 'Close room'}
            </Button>
          </AlertDialogTrigger>
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>Close this room?</AlertDialogTitle>
              <AlertDialogDescription className="leading-relaxed">{closeDescription}</AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel>Keep room open</AlertDialogCancel>
              <AlertDialogAction
                onClick={onCloseRoom}
                className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              >
                Close room
              </AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>
      </div>
    </div>
  );
}

const RoomControls: React.FC<RoomControlsProps> = React.memo((props) => {
  if (!props.roomCode) return <EmptyRoomControls {...props} />;
  return <ActiveRoomControls {...props} />;
});

RoomControls.displayName = 'RoomControls';

export const SourcePicker: React.FC<SourcePickerProps> = React.memo(
  ({
    roomCode,
    isCreatingRoom,
    isClosingRoom,
    isRoomTransitioning,
    canCreateRoom,
    roomCreateDisabledReason,
    hasRetryableRoom,
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
    onRetryRoomConnection,
    onCloseRoom,
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
            isClosingRoom={isClosingRoom || isRoomTransitioning}
            canCreateRoom={canCreateRoom}
            roomCreateDisabledReason={roomCreateDisabledReason}
            hasRetryableRoom={hasRetryableRoom}
            copied={copied}
            spectatorCount={spectatorCount}
            onCreateRoom={onCreateRoom}
            onRetryRoomConnection={onRetryRoomConnection}
            onCloseRoom={onCloseRoom}
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
                disabled={!canStartShare || isRoomTransitioning}
                aria-describedby={disabledReason ? 'start-screenshare-hint' : 'start-screenshare-ready-hint'}
                className="group relative w-full font-bold overflow-hidden shadow-[inset_0_1px_0_rgba(255,255,255,0.14)] hover:shadow-[inset_0_1px_0_rgba(255,255,255,0.18)] active:shadow-[inset_0_1px_1px_rgba(0,0,0,0.2)] active:scale-[0.99] transition-[transform,box-shadow,background-color] duration-200 ease-out disabled:shadow-none disabled:active:scale-100"
              >
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
                <span
                  aria-hidden="true"
                  className={`pointer-events-none absolute inset-0 -translate-x-full bg-gradient-to-r from-transparent via-white/[0.1] to-transparent opacity-0 transition-[transform,opacity] duration-[520ms] ease-[cubic-bezier(0.16,1,0.3,1)] group-hover:translate-x-full group-hover:opacity-100 group-focus-visible:translate-x-full group-focus-visible:opacity-100 motion-reduce:hidden ${!canStartShare ? 'hidden' : ''}`}
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
                <Button
                  variant="default"
                  onClick={onGoLive}
                  disabled={!canGoLive || isRoomTransitioning}
                  className="flex-1 font-bold"
                >
                  Go Live
                </Button>
                <Button variant="secondary" onClick={onStopShare} disabled={isRoomTransitioning} className="flex-1">
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
                <Button
                  variant="destructive"
                  onClick={() => setShowStopConfirm(true)}
                  disabled={isRoomTransitioning}
                  className="w-full font-bold"
                >
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
