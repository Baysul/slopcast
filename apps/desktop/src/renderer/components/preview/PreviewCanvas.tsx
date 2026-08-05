import { useEffect, useRef } from 'react';
import type { PreviewFrame } from '../../types';

// Live preview canvas: decodes each JPEG preview payload (encoded
// natively by libjpeg-turbo) via createImageBitmap and draws it on a plain
// 2D canvas, GPU-scaled to the card. Frame dimensions come from the decoded
// bitmap, so the canvas resizes with the capture source. Decoding happens
// off the main thread; a generation guard drops frames that finish decoding
// after a newer one has already been drawn.
export const PreviewCanvas: React.FC<{ frame: PreviewFrame }> = ({ frame }) => {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  // Incremented per received frame; a decode that completes after a newer
  // frame has been drawn (async decode reordering) is discarded.
  const drawGenRef = useRef(0);

  useEffect(() => {
    const gen = ++drawGenRef.current;
    const canvas = canvasRef.current;
    if (!canvas) return;
    const arrivalMs = performance.now();
    const decode = async (): Promise<void> => {
      try {
        const bitmap = await createImageBitmap(new Blob([frame.data], { type: 'image/jpeg' }));
        if (gen !== drawGenRef.current) {
          bitmap.close();
          return;
        }
        canvas.width = bitmap.width;
        canvas.height = bitmap.height;
        const ctx = canvas.getContext('2d');
        if (!ctx) {
          bitmap.close();
          return;
        }
        ctx.drawImage(bitmap, 0, 0);
        bitmap.close();
        if (window.__PREVIEW_BENCH__) {
          window.__PREVIEW_BENCH_DATA__?.push([frame.ptsUs, arrivalMs, performance.now()]);
        }
      } catch (err) {
        console.warn('[PreviewCanvas] failed to draw preview frame:', err);
      }
    };
    void decode();
  }, [frame]);

  return (
    <canvas
      ref={canvasRef}
      className="absolute inset-0 w-full h-full object-contain"
      aria-label="Live screenshare preview"
    />
  );
};
