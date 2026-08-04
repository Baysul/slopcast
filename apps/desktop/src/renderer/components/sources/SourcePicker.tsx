import React from 'react';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import type { CaptureContext, CaptureStage } from '../../types';

export interface SourcePickerProps {
  captureContext: CaptureContext | null;
  autoDetectFailed: boolean;
  captureStage: CaptureStage;
  showStopConfirm: boolean;
  setShowStopConfirm: (show: boolean) => void;
  spectatorCount: number;
  canStartShare: boolean;
  canGoLive: boolean;
  disabledReason: string | null;
  onStartShare: () => void;
  onGoLive: () => void;
  onStopShare: () => void;
}

export const SourcePicker: React.FC<SourcePickerProps> = React.memo(
  ({
    captureContext,
    autoDetectFailed,
    captureStage,
    showStopConfirm,
    setShowStopConfirm,
    spectatorCount,
    canStartShare,
    canGoLive,
    disabledReason,
    onStartShare,
    onGoLive,
    onStopShare,
  }) => {
    return (
      <Card>
        <CardHeader>
          <CardTitle className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
            Screenshare Source
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          {captureContext?.de === 'kde' && !autoDetectFailed && (
            <div className="space-y-2">
              <p className="text-sm text-muted-foreground bg-secondary border border-border rounded-lg p-3 leading-relaxed">
                KDE Plasma detected — window identity is unavailable in PipeWire streams. If auto-detection fails,
                select an audio app manually.
              </p>
            </div>
          )}

          {autoDetectFailed && captureContext?.de === 'kde' && (
            <div className="bg-secondary border border-border rounded-lg p-3 space-y-1">
              <p className="text-xs font-semibold text-foreground">KDE Audio Auto-Detection Failed</p>
              <p className="text-sm text-muted-foreground leading-relaxed">
                Select an audio app from the panel above, then stop and restart the screenshare.
              </p>
            </div>
          )}

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
