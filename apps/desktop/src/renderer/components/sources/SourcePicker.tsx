import React from 'react';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import type { CaptureContext, DesktopSource } from '../../types';

export interface SourcePickerProps {
  isWayland: boolean;
  desktopSources: DesktopSource[];
  selectedSourceId: string;
  onSelectSource: (source: DesktopSource) => void;
  captureContext: CaptureContext | null;
  autoDetectFailed: boolean;
  isSharing: boolean;
  showStopConfirm: boolean;
  setShowStopConfirm: (show: boolean) => void;
  spectatorCount: number;
  canStartShare: boolean;
  disabledReason: string | null;
  onStartShare: () => void;
  onStopShare: () => void;
}

const DesktopSourceCard: React.FC<{
  source: DesktopSource;
  isSelected: boolean;
  onSelect: (source: DesktopSource) => void;
}> = React.memo(({ source, isSelected, onSelect }) => (
  <button
    type="button"
    onClick={() => onSelect(source)}
    aria-label={source.name}
    className={`p-2 rounded-lg border cursor-pointer transition-all text-xs text-center space-y-1.5 w-full focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background ${
      isSelected
        ? 'bg-secondary border-input ring-1 ring-input/30'
        : 'bg-background/60 border-border hover:border-input'
    }`}
  >
    <img src={source.thumbnail} alt="" className="w-full h-20 object-cover rounded-md" aria-hidden="true" />
    <span className="block font-medium truncate text-foreground">{source.name}</span>
  </button>
));
DesktopSourceCard.displayName = 'DesktopSourceCard';

export const SourcePicker: React.FC<SourcePickerProps> = React.memo(
  ({
    isWayland,
    desktopSources,
    selectedSourceId,
    onSelectSource,
    captureContext,
    autoDetectFailed,
    isSharing,
    showStopConfirm,
    setShowStopConfirm,
    spectatorCount,
    canStartShare,
    disabledReason,
    onStartShare,
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
          {(() => {
            if (isWayland) {
              if (captureContext?.de === 'kde' && !autoDetectFailed) {
                return (
                  <div className="space-y-2">
                    <p className="text-sm text-muted-foreground bg-secondary border border-border rounded-lg p-3 leading-relaxed">
                      KDE Plasma detected — window identity is unavailable in PipeWire streams. If auto-detection fails,
                      select an audio app manually.
                    </p>
                  </div>
                );
              }
              return null;
            }
            return (
              <div className="grid grid-cols-2 gap-2 max-h-56 overflow-y-auto pr-1">
                {desktopSources.map((source) => (
                  <DesktopSourceCard
                    key={source.id}
                    source={source}
                    isSelected={source.id === selectedSourceId}
                    onSelect={onSelectSource}
                  />
                ))}
              </div>
            );
          })()}

          {autoDetectFailed && captureContext?.de === 'kde' && (
            <div className="bg-secondary border border-border rounded-lg p-3 space-y-1">
              <p className="text-xs font-semibold text-foreground">KDE Audio Auto-Detection Failed</p>
              <p className="text-sm text-muted-foreground leading-relaxed">
                Select an audio app from the panel above, then stop and restart the screenshare.
              </p>
            </div>
          )}

          {isSharing ? (
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
          ) : (
            <Button variant="default" onClick={onStartShare} disabled={!canStartShare} className="w-full font-bold">
              Start Screenshare
            </Button>
          )}
          {disabledReason && (
            <p id="start-screenshare-hint" className="text-sm text-muted-foreground leading-relaxed">
              {disabledReason}
            </p>
          )}
        </CardContent>
      </Card>
    );
  },
);

SourcePicker.displayName = 'SourcePicker';
