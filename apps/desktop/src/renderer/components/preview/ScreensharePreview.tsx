import { ScreenShare } from 'lucide-react';
import React, { useEffect, useRef } from 'react';
import { Card, CardContent } from '@/components/ui/card';
import { desktopApi } from '../../api/desktop';
import type { CaptureStage, PreviewFrame } from '../../types';
import { type StreamTelemetry, StreamTelemetryBar } from '../telemetry/StreamTelemetryBar';
import { PreviewCanvas } from './PreviewCanvas';

export interface ScreensharePreviewProps {
  captureStage: CaptureStage;
  roomCode: string;
  copied: 'link' | 'code' | null;
  previewFrame: PreviewFrame | null;
  telemetry: StreamTelemetry;
  onCopyLink: () => void;
}

// Debounce for reporting the preview card size to the backend: window
// resizes fire ResizeObserver callbacks in bursts, but the native scale
// target only needs the final size.
const VIEWPORT_REPORT_DEBOUNCE_MS = 150;

// Capture and encoding run entirely in native code (PipeWire -> native-livekit),
// so the renderer has no MediaStream to preview. While capture is active the
// card renders the JPEG preview frames pushed by the backend instead of a
// video element; telemetry overlays the canvas once the stream is live.
export const ScreensharePreview: React.FC<ScreensharePreviewProps> = React.memo(
  ({ captureStage, roomCode, copied, previewFrame, telemetry, onCopyLink }) => {
    const live = captureStage === 'live';
    const showPreview = previewFrame !== null && captureStage !== 'idle';
    const viewportRef = useRef<HTMLDivElement | null>(null);

    // Report the preview card size (device pixels) so the backend scales
    // preview frames to fit it — OBS-style "scale to the window". The canvas
    // is drawn into this container; the backend needs its size, not the
    // capture resolution, to size the JPEGs.
    useEffect(() => {
      const container = viewportRef.current;
      if (!container) return;
      let disposed = false;
      let timer: ReturnType<typeof setTimeout> | null = null;
      const report = (): void => {
        const rect = container.getBoundingClientRect();
        const width = Math.round(rect.width * window.devicePixelRatio);
        const height = Math.round(rect.height * window.devicePixelRatio);
        void desktopApi.setPreviewViewport(width, height);
      };
      const observer = new ResizeObserver(() => {
        if (timer) clearTimeout(timer);
        timer = setTimeout(() => {
          timer = null;
          if (!disposed) report();
        }, VIEWPORT_REPORT_DEBOUNCE_MS);
      });
      observer.observe(container);
      report();
      return () => {
        disposed = true;
        if (timer) clearTimeout(timer);
        observer.disconnect();
        void desktopApi.clearPreviewViewport();
      };
    }, []);

    return (
      <Card className="overflow-hidden shadow-2xl transition-all duration-300">
        <CardContent className="p-0">
          <div ref={viewportRef} className="relative bg-black aspect-video flex items-center justify-center">
            {showPreview && <PreviewCanvas frame={previewFrame} />}
            {live && <StreamTelemetryBar telemetry={telemetry} />}

            {captureStage === 'previewing' && (
              <div className="absolute top-3 left-3 z-10 flex items-center gap-1.5 rounded-full bg-black/60 backdrop-blur px-2.5 py-1">
                <span className="size-1.5 rounded-full bg-amber-400 motion-safe:animate-pulse" aria-hidden="true" />
                <span className="text-xs font-semibold uppercase tracking-wider text-foreground">Preview</span>
              </div>
            )}

            {captureStage === 'idle' && (
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
                    <p className="text-sm text-foreground font-semibold">Ready to go live</p>
                    <p className="text-sm text-muted-foreground max-w-xs leading-relaxed">
                      Click Start Screenshare below to begin broadcasting.
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

            {captureStage === 'previewing' && !showPreview && (
              <div className="absolute inset-0 flex flex-col items-center justify-center gap-3 text-center px-6">
                <span className="p-3 rounded-full bg-secondary">
                  <ScreenShare className="size-7 text-muted-foreground" aria-hidden="true" />
                </span>
                <p className="text-sm text-foreground font-semibold">Choosing a source…</p>
                <p className="text-sm text-muted-foreground max-w-xs leading-relaxed">
                  Pick what to share in the portal dialog to see a live preview.
                </p>
              </div>
            )}

            {live && !showPreview && (
              <div className="absolute inset-0 flex items-center justify-center">
                <span className="p-3 rounded-full bg-safelight/10">
                  <ScreenShare className="size-7 text-safelight" aria-hidden="true" />
                </span>
              </div>
            )}
          </div>
        </CardContent>
      </Card>
    );
  },
);

ScreensharePreview.displayName = 'ScreensharePreview';
