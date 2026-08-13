import type React from 'react';
import { useEffect, useRef } from 'react';

interface AudioVisualizerProps {
  mediaStream: MediaStream | null;
  showStatus?: boolean;
}

const AUDIO_UNLOCK_EVENT = 'slopcast-audio-unlock';

const CANVAS_WIDTH = 80;
const CANVAS_HEIGHT = 20;

export function unlockAudioContexts() {
  window.dispatchEvent(new CustomEvent(AUDIO_UNLOCK_EVENT));
}

const createAudioContext = (): AudioContext | null => {
  const ACtor = window.AudioContext ?? (window as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
  if (!ACtor) return null;
  const audioCtx = new ACtor();
  if (audioCtx.state === 'suspended') {
    audioCtx.resume().catch((err) => {
      console.warn('[AudioVisualizer] AudioContext resume failed:', err);
    });
  }
  return audioCtx;
};

const hasSignal = (data: Uint8Array<ArrayBuffer>): boolean => {
  for (const value of data) {
    if (value > 0) return true;
  }
  return false;
};

const drawBars = (ctx: CanvasRenderingContext2D, data: Uint8Array<ArrayBuffer>): void => {
  ctx.clearRect(0, 0, CANVAS_WIDTH, CANVAS_HEIGHT);
  const barWidth = (CANVAS_WIDTH / data.length) * 1.5;
  let x = 0;
  for (const value of data) {
    const barHeight = (value / 255) * CANVAS_HEIGHT;
    const alpha = 0.3 + (value / 255) * 0.7;
    ctx.fillStyle = `rgba(196, 128, 74, ${alpha})`;
    ctx.fillRect(x, CANVAS_HEIGHT - barHeight, barWidth - 2, barHeight);
    x += barWidth + 1;
  }
};

interface Pipeline {
  analyser: AnalyserNode;
  dataArray: Uint8Array<ArrayBuffer>;
  ctx: CanvasRenderingContext2D;
}

const initPipeline = (canvas: HTMLCanvasElement, audioCtx: AudioContext, mediaStream: MediaStream): Pipeline | null => {
  const analyser = audioCtx.createAnalyser();
  analyser.fftSize = 64;

  const source = audioCtx.createMediaStreamSource(mediaStream);
  source.connect(analyser);

  const dataArray = new Uint8Array(analyser.frequencyBinCount);

  const ctx = canvas.getContext('2d');
  if (!ctx) return null;

  // DPR-aware backing store so the 80×20 canvas is crisp on HiDPI
  // displays; the CSS size stays fixed.
  const dpr = window.devicePixelRatio || 1;
  canvas.width = CANVAS_WIDTH * dpr;
  canvas.height = CANVAS_HEIGHT * dpr;
  ctx.scale(dpr, dpr);

  return { analyser, dataArray, ctx };
};

const createDrawLoop = (pipeline: Pipeline, frameRef: { current: number }): (() => void) => {
  let wasSilent = false;
  const loop = () => {
    frameRef.current = requestAnimationFrame(loop);
    if (document.hidden) return;

    pipeline.analyser.getByteFrequencyData(pipeline.dataArray);

    if (!hasSignal(pipeline.dataArray)) {
      if (!wasSilent) {
        pipeline.ctx.clearRect(0, 0, CANVAS_WIDTH, CANVAS_HEIGHT);
        wasSilent = true;
      }
      return;
    }
    wasSilent = false;

    drawBars(pipeline.ctx, pipeline.dataArray);
  };
  return loop;
};

const startPipeline = (
  canvas: HTMLCanvasElement,
  mediaStream: MediaStream,
  frameRef: { current: number },
): AudioContext | null => {
  const audioCtx = createAudioContext();
  if (!audioCtx) return null;

  const pipeline = initPipeline(canvas, audioCtx, mediaStream);
  if (!pipeline) return null;

  createDrawLoop(pipeline, frameRef)();
  return audioCtx;
};

export const AudioVisualizer: React.FC<AudioVisualizerProps> = ({ mediaStream, showStatus }) => {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  useEffect(() => {
    if (!mediaStream || mediaStream.getAudioTracks().length === 0) return;

    const frameRef = { current: 0 };
    let audioCtx: AudioContext | null = null;
    let started = false;

    const startVisualizer = () => {
      if (started || !mediaStream || mediaStream.getAudioTracks().length === 0) return;

      const canvas = canvasRef.current;
      if (!canvas) return;

      try {
        const nextCtx = startPipeline(canvas, mediaStream, frameRef);
        if (!nextCtx) return;
        audioCtx = nextCtx;
        started = true;
      } catch (err) {
        console.error('[AudioVisualizer] Failed to initialize AudioContext:', err);
      }
    };

    startVisualizer();

    const onUnlock = () => {
      if (audioCtx && audioCtx.state === 'suspended') {
        audioCtx.resume().catch((err) => {
          console.warn('[AudioVisualizer] AudioContext resume failed:', err);
        });
      }
      startVisualizer();
    };
    window.addEventListener(AUDIO_UNLOCK_EVENT, onUnlock);

    return () => {
      window.removeEventListener(AUDIO_UNLOCK_EVENT, onUnlock);
      // cancelAnimationFrame(0) is a no-op when the pipeline never started.
      cancelAnimationFrame(frameRef.current);
      if (audioCtx && audioCtx.state !== 'closed') {
        audioCtx.close().catch((err) => {
          console.warn('[AudioVisualizer] AudioContext close failed:', err);
        });
      }
    };
  }, [mediaStream]);

  return (
    <div className="flex items-center gap-2 bg-black/30 backdrop-blur-sm px-3 py-1.5 rounded-lg border border-white/10">
      {showStatus && (
        <span className="relative w-1.5 h-1.5">
          <span className="absolute inset-0 rounded-full bg-safelight animate-ping opacity-75" />
          <span className="absolute inset-0 rounded-full bg-safelight" />
        </span>
      )}
      <span className="text-xs font-medium text-white/50 uppercase tracking-wider">Audio</span>
      <canvas ref={canvasRef} width={80} height={20} className="rounded overflow-hidden" />
    </div>
  );
};
