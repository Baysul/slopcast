import { ScreenShare } from 'lucide-react';
import React, { useEffect, useRef } from 'react';

import { Card, CardContent } from '@/components/ui/card';

import { desktopApi } from '../../../api/desktop';
import type { CaptureStage, PreviewFrame } from '../../../types';
import { StreamTelemetryBar } from '../../telemetry/StreamTelemetryBar';
import type { StreamTelemetry } from '../../telemetry/StreamTelemetryBar';
import { PreviewCanvas } from './PreviewCanvas';

export interface ScreensharePreviewProps {
  readonly captureStage: CaptureStage;
  readonly roomCode: string;
  readonly copied: 'link' | 'code' | null;
  readonly previewFrame: PreviewFrame | null;
  readonly telemetry: StreamTelemetry;
  readonly showTelemetry: boolean;
  readonly onCopyLink: () => void;
}

const VIEWPORT_REPORT_DEBOUNCE_MS = 150;

export const ScreensharePreview: React.FC<ScreensharePreviewProps> = React.memo(
  ({ captureStage, roomCode, copied, previewFrame, telemetry, showTelemetry, onCopyLink }: ScreensharePreviewProps) => {
    const live = captureStage === 'live';
    const showPreview = previewFrame !== null && captureStage !== 'idle';
    const viewportRef = useRef<HTMLDivElement | null>(null);
    let linkLabel = 'Copy invite link';
    let linkAriaLabel = 'Copy invite link';
    if (copied === 'link') {
      linkLabel = 'Link copied';
      linkAriaLabel = 'Invite link copied';
    }

    useEffect(() => {
      const container = viewportRef.current;
      if (!container) {
        return undefined;
      }
      let disposed = false;
      let timer: ReturnType<typeof setTimeout> | null = null;
      const report = (): void => {
        const rect = container.getBoundingClientRect();
        const width = Math.round(rect.width * window.devicePixelRatio);
        const height = Math.round(rect.height * window.devicePixelRatio);
        void desktopApi.setPreviewViewport(width, height);
      };
      const observer = new ResizeObserver(() => {
        if (timer !== null) {
          clearTimeout(timer);
        }
        timer = setTimeout(() => {
          timer = null;
          if (!disposed) {
            report();
          }
        }, VIEWPORT_REPORT_DEBOUNCE_MS);
      });
      observer.observe(container);
      report();
      return () => {
        disposed = true;
        if (timer !== null) {
          clearTimeout(timer);
        }
        observer.disconnect();
        void desktopApi.clearPreviewViewport();
      };
    }, []);

    return (
      // The parent is a size container; fitting by width or height keeps 16:9 inside it.
      <Card className="aspect-video w-[min(100cqw,calc(100cqh*16/9))] overflow-hidden shadow-2xl">
        <CardContent className="h-full p-0">
          <div ref={viewportRef} className="media-surface relative bg-black h-full flex items-center justify-center">
            {showPreview && <PreviewCanvas frame={previewFrame} />}
            {live && showTelemetry && <StreamTelemetryBar telemetry={telemetry} />}

            {captureStage === 'previewing' && (
              <div className="absolute top-3 left-3 z-10 flex items-center gap-1.5 rounded-full bg-black/60 backdrop-blur px-2.5 py-1">
                <span className="size-1.5 rounded-full bg-safelight motion-safe:animate-pulse" aria-hidden="true" />
                <span className="text-xs font-semibold uppercase tracking-wider text-foreground">Preview</span>
              </div>
            )}

            {captureStage === 'idle' && (
              <div className="absolute inset-0 flex flex-col items-center justify-center gap-3 text-center px-6">
                {roomCode && (
                  <>
                    <span className="p-3 rounded-full bg-secondary mb-1">
                      <ScreenShare className="size-7 text-muted-foreground" aria-hidden="true" />
                    </span>
                    <p className="text-sm text-foreground font-semibold">Ready to preview</p>
                    <p className="text-sm text-muted-foreground max-w-xs leading-relaxed">
                      Choose a window or screen to preview. You’ll go live from the preview.
                    </p>
                    <button
                      type="button"
                      onClick={onCopyLink}
                      aria-label={linkAriaLabel}
                      className="mt-1 inline-flex items-center gap-1.5 text-xs font-medium text-safelight hover:text-safelight-hover transition-colors focus:outline-none focus-visible:underline"
                    >
                      {linkLabel}
                    </button>
                  </>
                )}
                {!roomCode && (
                  <>
                    <span className="p-3 rounded-full bg-safelight/10 mb-1">
                      <ScreenShare className="size-7 text-safelight" aria-hidden="true" />
                    </span>
                    <p className="text-sm text-foreground font-semibold">No room yet</p>
                    <p className="text-sm text-muted-foreground max-w-xs leading-relaxed">
                      Create a live room to get your invite link, then pick a source to preview before you go live.
                    </p>
                  </>
                )}
              </div>
            )}

            {captureStage === 'previewing' && !showPreview && (
              <div className="absolute inset-0 flex flex-col items-center justify-center gap-3 text-center px-6">
                <span className="p-3 rounded-full bg-secondary">
                  <ScreenShare className="size-7 text-muted-foreground" aria-hidden="true" />
                </span>
                <p className="text-sm text-foreground font-semibold">Waiting for your selection…</p>
                <p className="text-sm text-muted-foreground max-w-xs leading-relaxed">
                  Select a window or screen in the system dialog. Your preview will appear here.
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
