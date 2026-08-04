import { useEffect, useRef } from 'react';
import type { PreviewFrame } from '../../types';

// Live preview canvas: decodes each base64 JPEG `preview-frame` payload into
// an ImageBitmap and draws it at full canvas resolution. The canvas CSS sizes
// the bitmap to the card; 640×360 @ 15 fps is trivial even on WebKitGTK's
// software canvas path (MIGRATION §9.2).
export const PreviewCanvas: React.FC<{ frame: PreviewFrame }> = ({ frame }) => {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  useEffect(() => {
    let cancelled = false;

    const draw = async (): Promise<void> => {
      try {
        const bytes = Uint8Array.from(atob(frame.data), (c) => c.charCodeAt(0));
        const blob = new Blob([bytes], { type: 'image/jpeg' });
        const bitmap = await createImageBitmap(blob);
        if (cancelled) {
          bitmap.close();
          return;
        }
        const canvas = canvasRef.current;
        const ctx = canvas?.getContext('2d');
        if (!canvas || !ctx) {
          bitmap.close();
          return;
        }
        canvas.width = frame.width;
        canvas.height = frame.height;
        ctx.drawImage(bitmap, 0, 0);
        bitmap.close();
      } catch (err) {
        console.warn('[PreviewCanvas] failed to draw preview frame:', err);
      }
    };

    void draw();
    return () => {
      cancelled = true;
    };
  }, [frame]);

  return (
    <canvas
      ref={canvasRef}
      className="absolute inset-0 w-full h-full object-contain"
      aria-label="Live screenshare preview"
    />
  );
};
