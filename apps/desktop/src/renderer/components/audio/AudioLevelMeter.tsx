import type React from 'react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { audioWaveStore, silentWave, WAVE_COLUMN_COUNT, waveIsActive } from '../../utils/audio-level-store';

const DEFAULT_WIDTH = 96;
const DEFAULT_HEIGHT = 20;

const SLEEP_COOLDOWN_MS = 300;

const FRAME_INTERVAL_MS = 1000 / 30;

export interface AudioLevelMeterProps {
  appId?: number;
  memberIds?: number[];
  width?: number;
  height?: number;
  className?: string;
}

function resolveIds(appId?: number, memberIds?: number[]): number[] {
  if (memberIds && memberIds.length > 0) return memberIds;
  if (appId !== undefined) return [appId];
  return [];
}

type MeterDraw = (now: number) => boolean;

const activeMeters = new Set<MeterDraw>();
let tickerFrame: number | null = null;
let lastTickTime = 0;

const tick = (now: number) => {
  if (now - lastTickTime >= FRAME_INTERVAL_MS) {
    lastTickTime = now;
    for (const draw of activeMeters) {
      if (!draw(now)) {
        activeMeters.delete(draw);
      }
    }
  }
  tickerFrame = activeMeters.size > 0 ? requestAnimationFrame(tick) : null;
};

function wakeMeter(draw: MeterDraw): void {
  activeMeters.add(draw);
  if (tickerFrame === null) {
    lastTickTime = 0;
    tickerFrame = requestAnimationFrame(tick);
  }
}

function sleepMeter(draw: MeterDraw): void {
  activeMeters.delete(draw);
  if (activeMeters.size === 0 && tickerFrame !== null) {
    cancelAnimationFrame(tickerFrame);
    tickerFrame = null;
  }
}

function prepareCanvas(canvas: HTMLCanvasElement, width: number, height: number): CanvasRenderingContext2D | null {
  const dpr = window.devicePixelRatio || 1;
  if (canvas.width !== width * dpr || canvas.height !== height * dpr) {
    canvas.width = width * dpr;
    canvas.height = height * dpr;
  }
  const ctx = canvas.getContext('2d');
  if (ctx) {
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, width, height);
  }
  return ctx;
}

const DISPLAY_BARS = 48;
const BAR_GAP = 0.5;

const PEAK_FLOOR_DB = 48;

function peakToHeight(peak: number): number {
  if (peak <= 0.000_01) return 0;
  const db = 20 * Math.log10(peak);
  if (db <= -PEAK_FLOOR_DB) return 0;
  if (db >= 0) return 1;
  return (db + PEAK_FLOOR_DB) / PEAK_FLOOR_DB;
}

function columnPeaks(columns: number[]): number[] {
  const pairs = Math.min(Math.floor(columns.length / 2), WAVE_COLUMN_COUNT);
  const bars = Math.max(1, Math.min(DISPLAY_BARS, Math.floor(pairs / 2)));
  const peaks = new Array<number>(bars).fill(0);
  for (let b = 0; b < bars; b++) {
    let peak = 0;
    for (let c = 0; c < 2; c++) {
      const idx = b * 2 + c;
      if (idx >= pairs) break;
      const min = columns[idx * 2] ?? 0;
      const max = columns[idx * 2 + 1] ?? 0;
      const p = Math.max(Math.abs(min), Math.abs(max));
      if (p > peak) {
        peak = p;
      }
    }
    peaks[b] = peakToHeight(peak);
  }
  return peaks;
}

const BAR_RELEASE_RATE = 3.5;

function advanceEnvelope(peaks: number[], envelope: number[], dt: number): void {
  for (let b = 0; b < envelope.length; b++) {
    const target = peaks[b] ?? 0;
    const current = envelope[b] ?? 0;
    if (target > current) {
      envelope[b] = target;
    } else {
      const released = current * Math.exp(-BAR_RELEASE_RATE * dt);
      envelope[b] = released;
      if (released < 0.001) {
        envelope[b] = 0;
      }
    }
  }
}

function paintPeakBars(ctx: CanvasRenderingContext2D, envelope: number[], width: number, height: number): void {
  const bars = envelope.length;
  const barWidth = (width - BAR_GAP * (bars - 1)) / bars;
  const center = height / 2;
  const maxHalf = height * 0.42;
  const radius = Math.min(barWidth / 2, 2);

  ctx.fillStyle = 'rgba(255, 255, 255, 0.08)';
  ctx.fillRect(0, center - 0.5, width, 1);

  for (let b = 0; b < bars; b++) {
    const scaled = envelope[b] ?? 0;
    if (scaled <= 0.001) continue;

    const half = Math.max(1.0, scaled * maxHalf);
    const fillAlpha = 0.5 + Math.min(1, scaled * 2) * 0.35;
    ctx.fillStyle = `rgba(196, 128, 74, ${fillAlpha})`;
    ctx.beginPath();
    ctx.roundRect(b * (barWidth + BAR_GAP), center - half, barWidth, half * 2, radius);
    ctx.fill();
  }
}

function unionColumns(merged: number[], columns: number[]): void {
  const pairs = Math.min(Math.floor(columns.length / 2), WAVE_COLUMN_COUNT);
  for (let i = 0; i < pairs * 2; i++) {
    const v = columns[i] ?? 0;
    const current = merged[i] ?? 0;
    if (i % 2 === 0) {
      if (v < current) {
        merged[i] = v;
      }
    } else if (v > current) {
      merged[i] = v;
    }
  }
}

function mergeWaves(members: Map<number, number[]>): number[] {
  const merged = silentWave();
  let first = true;
  for (const columns of members.values()) {
    const pairs = Math.min(Math.floor(columns.length / 2), WAVE_COLUMN_COUNT);
    if (first) {
      for (let i = 0; i < pairs * 2; i++) {
        merged[i] = columns[i] ?? 0;
      }
      first = false;
      continue;
    }
    unionColumns(merged, columns);
  }
  return merged;
}

function useWaveTarget(idsKey: string, onWave: (columns: number[]) => void): void {
  const memberWavesRef = useRef<Map<number, number[]>>(new Map());
  const onWaveRef = useRef(onWave);
  useEffect(() => {
    onWaveRef.current = onWave;
  });

  useEffect(() => {
    if (idsKey === '') return;
    const ids = idsKey.split(',').map(Number);

    const unsubs = ids.map((id) =>
      audioWaveStore.subscribe(id, (columns) => {
        memberWavesRef.current.set(id, columns);
        onWaveRef.current(mergeWaves(memberWavesRef.current));
      }),
    );

    return () => {
      for (const unsub of unsubs) unsub();
      memberWavesRef.current.clear();
    };
  }, [idsKey]);
}

function useMeterDraw(draw: MeterDraw): { wake: () => void } {
  const drawImplRef = useRef<MeterDraw>(() => false);
  useEffect(() => {
    drawImplRef.current = draw;
  });

  const stableDrawRef = useRef<MeterDraw | null>(null);
  if (stableDrawRef.current === null) {
    stableDrawRef.current = (now) => drawImplRef.current(now);
  }

  useEffect(() => {
    const stable = stableDrawRef.current;
    return () => {
      if (stable) sleepMeter(stable);
    };
  }, []);

  const wake = useCallback(() => {
    if (stableDrawRef.current) wakeMeter(stableDrawRef.current);
  }, []);

  return { wake };
}

function useMeterVisibility(): { hidden: boolean; signalActivity: (active: boolean) => void } {
  const [hidden, setHidden] = useState(true);
  const hiddenRef = useRef(true);
  const hideTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(
    () => () => {
      if (hideTimerRef.current) clearTimeout(hideTimerRef.current);
    },
    [],
  );

  const signalActivity = useCallback((active: boolean) => {
    if (active) {
      if (hideTimerRef.current) {
        clearTimeout(hideTimerRef.current);
        hideTimerRef.current = null;
      }
      if (hiddenRef.current) {
        hiddenRef.current = false;
        setHidden(false);
      }
    } else if (!hiddenRef.current && !hideTimerRef.current) {
      hideTimerRef.current = setTimeout(() => {
        hideTimerRef.current = null;
        hiddenRef.current = true;
        setHidden(true);
      }, SLEEP_COOLDOWN_MS);
    }
  }, []);

  return { hidden, signalActivity };
}

export const AudioLevelMeter: React.FC<AudioLevelMeterProps> = ({
  appId,
  memberIds,
  width = DEFAULT_WIDTH,
  height = DEFAULT_HEIGHT,
  className = '',
}) => {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const targetColumnsRef = useRef<number[]>(silentWave());
  const barEnvelopeRef = useRef<number[]>(new Array(DISPLAY_BARS).fill(0));
  const lastActiveAtRef = useRef<number>(0);
  const lastTimeRef = useRef<number | null>(null);
  const { hidden, signalActivity } = useMeterVisibility();

  const draw = (now: number): boolean => {
    const canvas = canvasRef.current;
    if (!canvas) return false;
    const ctx = prepareCanvas(canvas, width, height);
    if (!ctx) return false;

    const last = lastTimeRef.current ?? now;
    const dt = Math.min(0.5, Math.max(0.001, (now - last) / 1000));
    lastTimeRef.current = now;

    const peaks = columnPeaks(targetColumnsRef.current);
    let active = false;
    for (const p of peaks) {
      if (p > 0.001) {
        active = true;
        break;
      }
    }
    advanceEnvelope(peaks, barEnvelopeRef.current, dt);
    paintPeakBars(ctx, barEnvelopeRef.current, width, height);

    if (active) lastActiveAtRef.current = now;
    return active || now - lastActiveAtRef.current < SLEEP_COOLDOWN_MS;
  };

  const { wake } = useMeterDraw(draw);
  const idsKey = resolveIds(appId, memberIds).join(',');

  useWaveTarget(idsKey, (columns) => {
    targetColumnsRef.current = columns;
    const active = waveIsActive(columns);
    signalActivity(active);
    if (active) {
      wake();
    }
  });

  return (
    <canvas ref={canvasRef} style={{ width, height }} className={`shrink-0 ${hidden ? 'hidden' : ''} ${className}`} />
  );
};
