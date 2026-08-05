import { useEffect, useRef } from 'react';
import type { PreviewFrame } from '../../types';
import { drawI420Frame, parseI420Frame } from '../../utils/yuv';

// Live preview canvas: decodes each raw I420 `preview-frame` payload and
// draws it via WebGL2 (Y/U/V planes uploaded as R8 textures, YUV→RGB in the
// fragment shader, GPU-scaled to the card). No image codec is involved — the
// preview shows the actual capture planes at the stream's framerate.
export const PreviewCanvas: React.FC<{ frame: PreviewFrame }> = ({ frame }) => {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    try {
      canvas.width = frame.width;
      canvas.height = frame.height;
      drawI420Frame(canvas, parseI420Frame(frame.data, frame.width, frame.height));
    } catch (err) {
      console.warn('[PreviewCanvas] failed to draw preview frame:', err);
    }
  }, [frame]);

  return (
    <canvas
      ref={canvasRef}
      className="absolute inset-0 w-full h-full object-contain"
      aria-label="Live screenshare preview"
    />
  );
};
