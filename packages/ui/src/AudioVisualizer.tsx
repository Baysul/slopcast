import type React from 'react';
import { useCallback, useEffect, useRef, useState } from 'react';

const DEFAULT_WIDTH = 96;
const DEFAULT_HEIGHT = 20;
const DISPLAY_BARS = 48;
const BAR_GAP = 0.5;
const PEAK_FLOOR_DB = 48;
const BAR_RELEASE_RATE = 3.5;
const ACTIVE_AMPLITUDE = 0.002;
const SLEEP_COOLDOWN_MS = 300;
const FRAME_INTERVAL_MS = 1000 / 30;

export type AudioLevelListener = (levels: ArrayLike<number>) => void;
export type AudioLevelSubscription = (listener: AudioLevelListener) => () => void;

export interface AudioVisualizerProps {
  subscribe: AudioLevelSubscription;
  width?: number;
  height?: number;
  className?: string;
  hideWhenSilent?: boolean;
}

type VisualizerDraw = (now: number) => boolean;

const activeVisualizers = new Set<VisualizerDraw>();
let tickerFrame: number | null = null;
let lastTickTime = 0;

const tick = (now: number): void => {
  if (now - lastTickTime >= FRAME_INTERVAL_MS) {
    lastTickTime = now;
    for (const draw of activeVisualizers) {
      if (!draw(now)) activeVisualizers.delete(draw);
    }
  }
  tickerFrame = activeVisualizers.size > 0 ? requestAnimationFrame(tick) : null;
};

const wakeVisualizer = (draw: VisualizerDraw): void => {
  activeVisualizers.add(draw);
  if (tickerFrame !== null) return;

  lastTickTime = 0;
  tickerFrame = requestAnimationFrame(tick);
};

const sleepVisualizer = (draw: VisualizerDraw): void => {
  activeVisualizers.delete(draw);
  if (activeVisualizers.size > 0 || tickerFrame === null) return;

  cancelAnimationFrame(tickerFrame);
  tickerFrame = null;
};

const prepareCanvas = (canvas: HTMLCanvasElement, width: number, height: number): CanvasRenderingContext2D | null => {
  const dpr = window.devicePixelRatio || 1;
  if (canvas.width !== width * dpr || canvas.height !== height * dpr) {
    canvas.width = width * dpr;
    canvas.height = height * dpr;
  }

  const context = canvas.getContext('2d');
  if (!context) return null;

  context.setTransform(dpr, 0, 0, dpr, 0, 0);
  context.clearRect(0, 0, width, height);
  return context;
};

const peakToHeight = (peak: number): number => {
  if (peak <= 0.000_01) return 0;

  const decibels = 20 * Math.log10(peak);
  if (decibels <= -PEAK_FLOOR_DB) return 0;
  if (decibels >= 0) return 1;
  return (decibels + PEAK_FLOOR_DB) / PEAK_FLOOR_DB;
};

const levelPeaks = (levels: readonly number[]): number[] => {
  const barCount = Math.max(1, Math.min(DISPLAY_BARS, Math.floor(levels.length / 2)));
  const peaks = new Array<number>(barCount).fill(0);
  for (let barIndex = 0; barIndex < barCount; barIndex += 1) {
    const start = Math.floor((barIndex * levels.length) / barCount);
    const end = Math.max(start + 1, Math.floor(((barIndex + 1) * levels.length) / barCount));
    let peak = 0;
    for (let levelIndex = start; levelIndex < end; levelIndex += 1) {
      peak = Math.max(peak, Math.abs(levels[levelIndex] ?? 0));
    }
    peaks[barIndex] = peakToHeight(peak);
  }
  return peaks;
};

const advanceEnvelope = (peaks: readonly number[], envelope: number[], elapsedSeconds: number): void => {
  for (let barIndex = 0; barIndex < envelope.length; barIndex += 1) {
    const target = peaks[barIndex] ?? 0;
    const current = envelope[barIndex] ?? 0;
    if (target > current) {
      envelope[barIndex] = target;
      continue;
    }

    const released = current * Math.exp(-BAR_RELEASE_RATE * elapsedSeconds);
    envelope[barIndex] = released < 0.001 ? 0 : released;
  }
};

const paintBars = (context: CanvasRenderingContext2D, envelope: readonly number[], width: number, height: number) => {
  const barWidth = (width - BAR_GAP * (envelope.length - 1)) / envelope.length;
  const center = height / 2;
  const maximumHalfHeight = height * 0.42;
  const radius = Math.min(barWidth / 2, 2);

  context.fillStyle = 'rgba(255, 255, 255, 0.08)';
  context.fillRect(0, center - 0.5, width, 1);
  for (let barIndex = 0; barIndex < envelope.length; barIndex += 1) {
    const level = envelope[barIndex] ?? 0;
    if (level <= 0.001) continue;

    const halfHeight = Math.max(1, level * maximumHalfHeight);
    const fillAlpha = 0.5 + Math.min(1, level * 2) * 0.35;
    context.fillStyle = `rgba(196, 128, 74, ${fillAlpha})`;
    context.beginPath();
    context.roundRect(barIndex * (barWidth + BAR_GAP), center - halfHeight, barWidth, halfHeight * 2, radius);
    context.fill();
  }
};

const levelsAreActive = (levels: readonly number[]): boolean =>
  levels.some((level) => Math.abs(level) > ACTIVE_AMPLITUDE);

const useStableDraw = (draw: VisualizerDraw): (() => void) => {
  const drawRef = useRef(draw);
  drawRef.current = draw;
  const stableDrawRef = useRef<VisualizerDraw | null>(null);
  if (stableDrawRef.current === null) stableDrawRef.current = (now) => drawRef.current(now);

  useEffect(() => {
    const stableDraw = stableDrawRef.current;
    return () => {
      if (stableDraw) sleepVisualizer(stableDraw);
    };
  }, []);

  return useCallback(() => {
    if (stableDrawRef.current) wakeVisualizer(stableDrawRef.current);
  }, []);
};

export const AudioVisualizer: React.FC<AudioVisualizerProps> = ({
  subscribe,
  width = DEFAULT_WIDTH,
  height = DEFAULT_HEIGHT,
  className = '',
  hideWhenSilent = false,
}) => {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const targetLevelsRef = useRef<number[]>([]);
  const envelopeRef = useRef<number[]>(new Array(DISPLAY_BARS).fill(0));
  const lastActiveAtRef = useRef(0);
  const lastDrawAtRef = useRef<number | null>(null);
  const isSilentRef = useRef(true);
  const hideTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [isSilent, setIsSilent] = useState(true);

  const signalActivity = useCallback((isActive: boolean): void => {
    if (isActive) {
      if (hideTimerRef.current) clearTimeout(hideTimerRef.current);
      hideTimerRef.current = null;
      if (!isSilentRef.current) return;

      isSilentRef.current = false;
      setIsSilent(false);
      return;
    }
    if (isSilentRef.current || hideTimerRef.current) return;

    hideTimerRef.current = setTimeout(() => {
      hideTimerRef.current = null;
      isSilentRef.current = true;
      setIsSilent(true);
    }, SLEEP_COOLDOWN_MS);
  }, []);

  const draw = (now: number): boolean => {
    const canvas = canvasRef.current;
    if (!canvas) return false;

    const context = prepareCanvas(canvas, width, height);
    if (!context) return false;

    const lastDrawAt = lastDrawAtRef.current ?? now;
    const elapsedSeconds = Math.min(0.5, Math.max(0.001, (now - lastDrawAt) / 1000));
    const peaks = levelPeaks(targetLevelsRef.current);
    const isActive = levelsAreActive(peaks);
    lastDrawAtRef.current = now;
    advanceEnvelope(peaks, envelopeRef.current, elapsedSeconds);
    paintBars(context, envelopeRef.current, width, height);
    if (isActive) lastActiveAtRef.current = now;
    return isActive || now - lastActiveAtRef.current < SLEEP_COOLDOWN_MS;
  };
  const wake = useStableDraw(draw);

  useEffect(() => {
    targetLevelsRef.current = [];
    envelopeRef.current.fill(0);
    lastDrawAtRef.current = null;
    isSilentRef.current = true;
    setIsSilent(true);
    const unsubscribe = subscribe((levels) => {
      const nextLevels = Array.from(levels);
      const isActive = levelsAreActive(nextLevels);
      targetLevelsRef.current = nextLevels;
      signalActivity(isActive);
      if (isActive) wake();
    });
    wake();
    return () => {
      unsubscribe();
      if (hideTimerRef.current) clearTimeout(hideTimerRef.current);
      hideTimerRef.current = null;
    };
  }, [signalActivity, subscribe, wake]);

  const visibilityClass = hideWhenSilent && isSilent ? 'hidden' : '';
  return (
    <canvas
      ref={canvasRef}
      width={width}
      height={height}
      style={{ width, height }}
      className={`${visibilityClass} ${className}`}
    />
  );
};
