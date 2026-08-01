import { ScreenShare } from 'lucide-react';
import type React from 'react';
import { useEffect, useRef } from 'react';
import { Card, CardContent } from '@/components/ui/card';
import { type StreamTelemetry, StreamTelemetryBar } from '../telemetry/StreamTelemetryBar';

export interface ScreensharePreviewProps {
  previewStream: MediaStream | null;
  isSharing: boolean;
  roomCode: string;
  canStartShare: boolean;
  copied: 'link' | 'code' | null;
  telemetry: StreamTelemetry;
  onCopyLink: () => void;
}

export const ScreensharePreview: React.FC<ScreensharePreviewProps> = ({
  previewStream,
  isSharing,
  roomCode,
  canStartShare,
  copied,
  telemetry,
  onCopyLink,
}) => {
  const previewVideoRef = useRef<HTMLVideoElement | null>(null);

  useEffect(() => {
    const el = previewVideoRef.current;
    if (!el) return;
    el.srcObject = previewStream;
    if (previewStream) {
      el.muted = true;
      el.play().catch(() => console.warn('Video autoplay blocked until user gesture'));
    }
  }, [previewStream]);

  return (
    <Card className="overflow-hidden shadow-2xl transition-all duration-300">
      <CardContent className="p-0">
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
            <div className="absolute inset-0 flex flex-col items-center justify-center gap-3 text-center px-6">
              {!roomCode ? (
                <>
                  <span className="p-3 rounded-full bg-safelight/10 mb-1">
                    <ScreenShare className="size-7 text-safelight/60" aria-hidden="true" />
                  </span>
                  <p className="text-sm text-foreground font-semibold">Ready to stream</p>
                  <p className="text-sm text-muted-foreground max-w-xs leading-relaxed">
                    Create a live room to get a shareable link, then select your source and go live.
                  </p>
                </>
              ) : (
                <>
                  <span className="p-3 rounded-full bg-secondary mb-1">
                    <ScreenShare className="size-7 text-muted-foreground" aria-hidden="true" />
                  </span>
                  <p className="text-sm text-foreground font-semibold">
                    {canStartShare ? 'Ready to go live' : 'Select a source to begin'}
                  </p>
                  <p className="text-sm text-muted-foreground max-w-xs leading-relaxed">
                    {!canStartShare
                      ? 'Choose a window or screen in the Screenshare Source panel.'
                      : 'Click Start Screenshare below to begin broadcasting.'}
                  </p>
                  <button
                    type="button"
                    onClick={onCopyLink}
                    className="mt-1 inline-flex items-center gap-1.5 text-xs font-medium text-safelight hover:text-safelight-hover transition-colors focus:outline-none focus-visible:underline"
                  >
                    {copied === 'link' ? 'Link copied' : `Copy link — ${roomCode}`}
                  </button>
                </>
              )}
            </div>
          )}
          {isSharing && <StreamTelemetryBar telemetry={telemetry} />}
        </div>
      </CardContent>
    </Card>
  );
};
