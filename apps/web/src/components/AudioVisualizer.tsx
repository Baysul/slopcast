import type React from 'react';
import { useEffect, useRef, useState } from 'react';

declare global {
  interface Window {
    webkitAudioContext?: typeof AudioContext;
  }
}

interface AudioVisualizerProps {
  mediaStream: MediaStream | null;
  playerRef: React.RefObject<HTMLDivElement | null>;
  className?: string;
  showStatus?: boolean;
}

interface RelativePosition {
  x: number;
  y: number;
}

interface DragSession {
  pointerId: number;
  startX: number;
  startY: number;
  grabOffsetX: number;
  grabOffsetY: number;
  hasMoved: boolean;
}

const AUDIO_UNLOCK_EVENT = 'slopcast-audio-unlock';
const POSITION_STORAGE_KEY = 'slopcast:audio-visualizer-position';
const DRAG_THRESHOLD = 3;
const DOUBLE_TAP_DELAY_MS = 350;

const CANVAS_WIDTH = 80;
const CANVAS_HEIGHT = 20;

export function unlockAudioContexts() {
  window.dispatchEvent(new CustomEvent(AUDIO_UNLOCK_EVENT));
}

const isRelativePosition = (position: RelativePosition): boolean =>
  Number.isFinite(position.x) &&
  Number.isFinite(position.y) &&
  position.x >= 0 &&
  position.x <= 1 &&
  position.y >= 0 &&
  position.y <= 1;

const readSavedPosition = (): RelativePosition | null => {
  try {
    const storedPosition = window.localStorage.getItem(POSITION_STORAGE_KEY);
    if (!storedPosition) return null;

    const [xValue, yValue] = storedPosition.split(',');
    const position = { x: Number(xValue), y: Number(yValue) };
    return isRelativePosition(position) ? position : null;
  } catch (error) {
    console.info('[AudioVisualizer] Saved position is unavailable:', error);
    return null;
  }
};

const savePosition = (position: RelativePosition): void => {
  try {
    window.localStorage.setItem(POSITION_STORAGE_KEY, `${position.x},${position.y}`);
  } catch (error) {
    console.info('[AudioVisualizer] Position could not be saved:', error);
  }
};

const clearSavedPosition = (): void => {
  try {
    window.localStorage.removeItem(POSITION_STORAGE_KEY);
  } catch (error) {
    console.info('[AudioVisualizer] Saved position could not be cleared:', error);
  }
};

const clamp = (value: number, maximum: number): number => Math.min(maximum, Math.max(0, value));

const getPositionStyle = (position: RelativePosition | null): React.CSSProperties | undefined => {
  if (!position) return undefined;

  return {
    left: `${position.x * 100}%`,
    top: `${position.y * 100}%`,
    transform: `translate(${-position.x * 100}%, ${-position.y * 100}%)`,
  };
};

const createAudioContext = (): AudioContext | null => {
  const ACtor = window.AudioContext ?? window.webkitAudioContext;
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

export const AudioVisualizer: React.FC<AudioVisualizerProps> = ({ mediaStream, playerRef, className, showStatus }) => {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const visualizerRef = useRef<HTMLDivElement | null>(null);
  const dragSessionRef = useRef<DragSession | null>(null);
  const lastTapTimeRef = useRef(0);
  const [position, setPosition] = useState<RelativePosition | null>(readSavedPosition);
  const positionRef = useRef(position);

  const resetPosition = (): void => {
    positionRef.current = null;
    setPosition(null);
    clearSavedPosition();
  };

  const handlePointerDown = (event: React.PointerEvent<HTMLDivElement>): void => {
    if (event.button !== 0) return;

    const player = playerRef.current;
    const visualizer = visualizerRef.current;
    if (!player || !visualizer) return;

    const visualizerBounds = visualizer.getBoundingClientRect();
    dragSessionRef.current = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      grabOffsetX: event.clientX - visualizerBounds.left,
      grabOffsetY: event.clientY - visualizerBounds.top,
      hasMoved: false,
    };

    event.preventDefault();
    event.stopPropagation();
    event.currentTarget.setPointerCapture(event.pointerId);
  };

  const handlePointerMove = (event: React.PointerEvent<HTMLDivElement>): void => {
    const dragSession = dragSessionRef.current;
    if (!dragSession || dragSession.pointerId !== event.pointerId) return;

    const player = playerRef.current;
    const visualizer = visualizerRef.current;
    if (!player || !visualizer) return;

    const horizontalMovement = Math.abs(event.clientX - dragSession.startX);
    const verticalMovement = Math.abs(event.clientY - dragSession.startY);
    if (!dragSession.hasMoved && horizontalMovement <= DRAG_THRESHOLD && verticalMovement <= DRAG_THRESHOLD) return;

    const playerBounds = player.getBoundingClientRect();
    const visualizerBounds = visualizer.getBoundingClientRect();
    const maximumLeft = Math.max(0, playerBounds.width - visualizerBounds.width);
    const maximumTop = Math.max(0, playerBounds.height - visualizerBounds.height);
    const left = clamp(event.clientX - playerBounds.left - dragSession.grabOffsetX, maximumLeft);
    const top = clamp(event.clientY - playerBounds.top - dragSession.grabOffsetY, maximumTop);
    const nextPosition = {
      x: maximumLeft === 0 ? 0 : left / maximumLeft,
      y: maximumTop === 0 ? 0 : top / maximumTop,
    };

    dragSession.hasMoved = true;
    positionRef.current = nextPosition;
    setPosition(nextPosition);
    event.preventDefault();
    event.stopPropagation();
  };

  const finishDrag = (event: React.PointerEvent<HTMLDivElement>, shouldDetectTap: boolean): void => {
    const dragSession = dragSessionRef.current;
    if (!dragSession || dragSession.pointerId !== event.pointerId) return;

    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    dragSessionRef.current = null;
    event.stopPropagation();

    if (dragSession.hasMoved) {
      lastTapTimeRef.current = 0;
      if (positionRef.current) savePosition(positionRef.current);
      return;
    }
    if (!shouldDetectTap || event.pointerType === 'mouse') return;

    const tapTime = performance.now();
    if (tapTime - lastTapTimeRef.current <= DOUBLE_TAP_DELAY_MS) {
      lastTapTimeRef.current = 0;
      resetPosition();
      return;
    }
    lastTapTimeRef.current = tapTime;
  };

  const handleDoubleClick = (event: React.MouseEvent<HTMLDivElement>): void => {
    event.preventDefault();
    event.stopPropagation();
    resetPosition();
  };

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
      cancelAnimationFrame(frameRef.current);
      if (audioCtx && audioCtx.state !== 'closed') {
        audioCtx.close().catch((err) => {
          console.warn('[AudioVisualizer] AudioContext close failed:', err);
        });
      }
    };
  }, [mediaStream]);

  const placementClass = position ? '' : 'top-4 right-16';

  return (
    <div
      ref={visualizerRef}
      data-audio-visualizer
      role="img"
      aria-label="Audio activity visualizer. Drag to move. Double-click or double-tap to reset."
      title="Drag to move. Double-click or double-tap to reset."
      className={`${className ?? ''} ${placementClass} flex touch-none cursor-grab active:cursor-grabbing items-center gap-2 rounded-lg border border-white/10 bg-black/30 px-3 py-1.5 backdrop-blur-sm`}
      style={getPositionStyle(position)}
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={(event) => finishDrag(event, true)}
      onPointerCancel={(event) => finishDrag(event, false)}
      onDoubleClick={handleDoubleClick}
    >
      {showStatus && (
        <span aria-hidden="true" className="relative w-1.5 h-1.5">
          <span className="absolute inset-0 rounded-full bg-safelight animate-ping opacity-75" />
          <span className="absolute inset-0 rounded-full bg-safelight" />
        </span>
      )}
      <span className="text-xs font-medium text-white/50 uppercase tracking-wider">Audio</span>
      <canvas ref={canvasRef} width={80} height={20} className="rounded overflow-hidden" />
    </div>
  );
};
