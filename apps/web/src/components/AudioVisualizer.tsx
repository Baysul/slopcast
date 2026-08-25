import { type AudioLevelSubscription, AudioVisualizer as SharedAudioVisualizer } from '@slopcast/ui';
import type React from 'react';
import { useMemo, useRef, useState } from 'react';

declare global {
  interface Window {
    webkitAudioContext?: typeof AudioContext;
  }
}

interface AudioVisualizerProps {
  mediaStream: MediaStream;
  playerRef: React.RefObject<HTMLDivElement | null>;
  className?: string;
  showStatus?: boolean;
  onInteraction?: (() => void) | undefined;
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
const SAMPLE_INTERVAL_MS = 1000 / 30;

export const unlockAudioContexts = (): void => {
  window.dispatchEvent(new CustomEvent(AUDIO_UNLOCK_EVENT));
};

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

const createMediaStreamSubscription =
  (mediaStream: MediaStream): AudioLevelSubscription =>
  (listener) => {
    const AudioContextConstructor = window.AudioContext ?? window.webkitAudioContext;
    if (!AudioContextConstructor || mediaStream.getAudioTracks().length === 0) return () => undefined;

    let audioContext: AudioContext;
    try {
      audioContext = new AudioContextConstructor();
    } catch (error) {
      console.error('[AudioVisualizer] Failed to initialize AudioContext:', error);
      return () => undefined;
    }

    const analyser = audioContext.createAnalyser();
    const source = audioContext.createMediaStreamSource(mediaStream);
    const levels = new Float32Array(256);
    let animationFrame = 0;
    let lastSampleAt = 0;
    analyser.fftSize = levels.length;
    source.connect(analyser);

    const resumeAudio = (): void => {
      if (audioContext.state !== 'suspended') return;
      audioContext.resume().catch((error) => {
        console.warn('[AudioVisualizer] AudioContext resume failed:', error);
      });
    };
    const sample = (now: number): void => {
      animationFrame = requestAnimationFrame(sample);
      if (document.hidden || now - lastSampleAt < SAMPLE_INTERVAL_MS) return;

      lastSampleAt = now;
      analyser.getFloatTimeDomainData(levels);
      listener(levels);
    };

    resumeAudio();
    window.addEventListener(AUDIO_UNLOCK_EVENT, resumeAudio);
    animationFrame = requestAnimationFrame(sample);
    return () => {
      window.removeEventListener(AUDIO_UNLOCK_EVENT, resumeAudio);
      cancelAnimationFrame(animationFrame);
      source.disconnect();
      analyser.disconnect();
      if (audioContext.state === 'closed') return;
      audioContext.close().catch((error) => {
        console.warn('[AudioVisualizer] AudioContext close failed:', error);
      });
    };
  };

export const AudioVisualizer: React.FC<AudioVisualizerProps> = ({
  mediaStream,
  playerRef,
  className,
  showStatus,
  onInteraction,
}) => {
  const visualizerRef = useRef<HTMLDivElement | null>(null);
  const dragSessionRef = useRef<DragSession | null>(null);
  const lastTapTimeRef = useRef(0);
  const [position, setPosition] = useState<RelativePosition | null>(readSavedPosition);
  const positionRef = useRef(position);
  const subscribe = useMemo(() => createMediaStreamSubscription(mediaStream), [mediaStream]);

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
    onInteraction?.();
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
    onInteraction?.();
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
      <SharedAudioVisualizer subscribe={subscribe} />
    </div>
  );
};
