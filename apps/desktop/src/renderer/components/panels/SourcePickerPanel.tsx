import type { AudioApp } from '@slopcast/shared-types';
import type React from 'react';
import type { CaptureContext, DesktopSource } from '../../types';

export interface SourcePickerPanelProps {
  isWayland: boolean;
  desktopSources: DesktopSource[];
  selectedSourceId: string;
  captureContext: CaptureContext | null;
  autoDetectFailed: boolean;
  isSharing: boolean;
  canStartShare: boolean;
  shareButtonClass: string;
  disabledReason: string | null;
  attemptAutoResolve: (opts?: { sourceId?: string }) => Promise<AudioApp | null>;
  setSelectedSourceId: (id: string) => void;
  handleStartShare: () => void;
  handleStopShare: () => void;
}

export const SourcePickerPanel: React.FC<SourcePickerPanelProps> = ({
  isWayland,
  desktopSources,
  selectedSourceId,
  captureContext,
  autoDetectFailed,
  isSharing,
  canStartShare,
  shareButtonClass,
  disabledReason,
  attemptAutoResolve,
  setSelectedSourceId,
  handleStartShare,
  handleStopShare,
}) => (
  <div className="bg-gray-900/80 border border-gray-800 rounded-xl p-6 space-y-4">
    <h2 className="text-xs font-semibold uppercase tracking-wider text-gray-400">Screenshare Source</h2>

    {isWayland ? (
      <div className="space-y-2 text-xs">
        <p className="text-gray-500 leading-relaxed">
          The system dialog (xdg-desktop-portal) will let you pick the window to share. Audio is auto-detected via
          PipeWire introspection.
        </p>
        {captureContext?.de === 'kde' && !autoDetectFailed && (
          <p className="text-gray-400 bg-gray-800/40 border border-gray-700/40 rounded-lg p-2.5 leading-relaxed">
            KDE Plasma detected \u2014 window identity is unavailable in PipeWire streams. If auto-detection fails,
            select an audio app manually.
          </p>
        )}
      </div>
    ) : (
      <div className="grid grid-cols-2 gap-2 max-h-56 overflow-y-auto pr-1">
        {desktopSources.map((source) => {
          const isSelected = source.id === selectedSourceId;
          return (
            <button
              key={source.id}
              type="button"
              onClick={() => {
                setSelectedSourceId(source.id);
                void attemptAutoResolve({ sourceId: source.id });
              }}
              aria-label={source.name}
              className={`p-2 rounded-lg border cursor-pointer transition-all text-xs text-center space-y-1.5 w-full focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background ${
                isSelected
                  ? 'bg-gray-800/50 border-gray-600 ring-1 ring-gray-600/30'
                  : 'bg-background/60 border-gray-800/60 hover:border-gray-700'
              }`}
            >
              <img src={source.thumbnail} alt="" className="w-full h-20 object-cover rounded" aria-hidden="true" />
              <span className="block font-medium truncate text-gray-300">{source.name}</span>
            </button>
          );
        })}
      </div>
    )}

    {autoDetectFailed && captureContext?.de === 'kde' && (
      <div className="bg-gray-800/50 border border-gray-700/50 rounded-lg p-3 space-y-1">
        <p className="text-xs font-semibold text-gray-200">KDE Audio Auto-Detection Failed</p>
        <p className="text-[11px] text-gray-500 leading-relaxed">
          Select an audio app from the panel above, then stop and restart the screenshare.
        </p>
      </div>
    )}

    <button
      type="button"
      onClick={isSharing ? handleStopShare : handleStartShare}
      disabled={!isSharing && !canStartShare}
      title={isSharing ? 'Stop the broadcast and disconnect all spectators.' : undefined}
      aria-describedby={disabledReason ? 'start-screenshare-hint' : undefined}
      className={`w-full py-3 text-sm font-bold rounded-lg transition-all disabled:opacity-50 disabled:cursor-not-allowed focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight focus-visible:ring-offset-2 focus-visible:ring-offset-background ${shareButtonClass}`}
    >
      {isSharing ? 'Stop Screenshare' : 'Start Screenshare'}
    </button>
    {disabledReason && (
      <p id="start-screenshare-hint" className="text-[11px] text-gray-500 leading-relaxed">
        {disabledReason}
      </p>
    )}
  </div>
);
