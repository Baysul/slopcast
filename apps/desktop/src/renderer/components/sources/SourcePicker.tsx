import { Check, Copy, X } from 'lucide-react';
import React, { useState } from 'react';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import type { CaptureContext, CaptureSourceSelection, CaptureStage } from '../../types';
import { CaptureSourcePicker } from './CaptureSourcePicker';

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
        <Button variant="default" onClick={onCreateRoom} disabled={isCreatingRoom} className="w-full font-bold">
          {isCreatingRoom ? 'Creating Room...' : 'Create Live Room'}
        </Button>
      );
    }

    return (
      <div className="space-y-3">
        <div className="flex items-center gap-2 flex-wrap">
          {spectatorCount > 0 && (
            <Badge variant="info" className="tabular-nums">
              {spectatorCount} spectator{spectatorCount === 1 ? '' : 's'}
            </Badge>
          )}
          <span className="font-mono text-sm font-semibold tabular-nums tracking-wide text-foreground/90">
            {roomCode}
          </span>
          <Button variant="secondary" size="sm" onClick={onCopyCode} className="gap-2">
            <span className="text-foreground bg-accent/50 px-2 py-0.5 rounded-md text-xs flex items-center gap-1">
              {copied === 'code' ? (
                <>
                  <Check className="w-3 h-3 text-safelight" aria-hidden="true" />
                  Copied
                </>
              ) : (
                'Copy'
              )}
            </span>
          </Button>
          <Button size="sm" onClick={onCopyLink} className="gap-1.5">
            {copied === 'link' ? (
              <Check className="w-4 h-4" aria-hidden="true" />
            ) : (
              <Copy className="w-4 h-4" aria-hidden="true" />
            )}
            {copied === 'link' ? 'Link Copied!' : 'Copy Link'}
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
  }) => {
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
            <>
              <Button variant="default" onClick={onStartShare} disabled={!canStartShare} className="w-full font-bold">
                Start Screenshare
              </Button>
              {disabledReason && (
                <p id="start-screenshare-hint" className="text-sm text-muted-foreground leading-relaxed">
                  {disabledReason}
                </p>
              )}
            </>
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
