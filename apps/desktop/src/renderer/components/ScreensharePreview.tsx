import type React from 'react';
import { StreamTelemetryBar } from '../telemetry/StreamTelemetryBar';
import type { StreamTelemetry } from '../telemetry/types';

export interface ScreensharePreviewProps {
  isSharing: boolean;
  previewVideoRef: React.RefObject<HTMLVideoElement | null>;
  telemetry: StreamTelemetry;
}

export const ScreensharePreview: React.FC<ScreensharePreviewProps> = ({ isSharing, previewVideoRef, telemetry }) => (
  <div className="bg-gray-900/80 border border-gray-800 rounded-xl overflow-hidden shadow-2xl">
    <div className="relative bg-black aspect-video flex items-center justify-center">
      <video
        ref={previewVideoRef}
        autoPlay
        playsInline
        muted
        aria-label="Screen share preview"
        className={`w-full h-full object-contain ${isSharing ? 'block' : 'hidden'}`}
      />
      {!isSharing && (
        <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 text-center px-6">
          <p className="text-sm text-gray-400 font-medium">No active screenshare</p>
          <p className="text-xs text-gray-600 max-w-sm">
            Select a window below and start sharing. Audio is auto-detected.
          </p>
        </div>
      )}
      {isSharing && <StreamTelemetryBar telemetry={telemetry} />}
    </div>
  </div>
);
