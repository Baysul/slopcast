import { useEffect, useRef, useState } from 'react';

import { desktopApi } from '../api/desktop';
import type { DesktopCaptureStats } from '../types';
import type { MotionMode, MotionTier } from '../utils/bitrate';

const POLL_INTERVAL_MS = 2000;
const DYNAMIC_THRESHOLD = 0.55;
const STATIC_THRESHOLD = 0.15;
const HYSTERESIS = 0.1;

export const classifyMotionTier = (motionRatio: number, previous: MotionTier): MotionTier => {
  if (previous === 'dynamic') {
    if (motionRatio >= DYNAMIC_THRESHOLD - HYSTERESIS) {
      return 'dynamic';
    }
    if (motionRatio < STATIC_THRESHOLD) {
      return 'static';
    }
    return 'mixed';
  }
  if (previous === 'static') {
    if (motionRatio <= STATIC_THRESHOLD + HYSTERESIS) {
      return 'static';
    }
    if (motionRatio > DYNAMIC_THRESHOLD) {
      return 'dynamic';
    }
    return 'mixed';
  }
  if (motionRatio < STATIC_THRESHOLD) {
    return 'static';
  }
  if (motionRatio > DYNAMIC_THRESHOLD) {
    return 'dynamic';
  }
  return 'mixed';
};

interface MotionDetection {
  readonly motionTier: MotionTier;
  readonly detected: boolean;
}

interface MotionSnapshot {
  readonly real: number;
  readonly keepalive: number;
}

interface MotionSample {
  readonly tier: MotionTier | null;
  readonly prev: MotionSnapshot;
}

const NO_FRAMES = 0;

const snapshot = (stats: DesktopCaptureStats): MotionSnapshot => ({
  keepalive: stats.keepalivePushed,
  real: stats.framesPushed,
});

const sampleMotion = async (prev: MotionSnapshot | null, currentTier: MotionTier): Promise<MotionSample> => {
  const stats = await desktopApi.getVideoCaptureStats();
  const current = snapshot(stats);
  if (!prev) {
    return { prev: current, tier: null };
  }
  const realDelta = current.real - prev.real;
  const keepaliveDelta = current.keepalive - prev.keepalive;
  const total = realDelta + keepaliveDelta;
  if (total <= NO_FRAMES) {
    return { prev: current, tier: null };
  }
  const ratio = realDelta / total;
  const next = classifyMotionTier(ratio, currentTier);
  let changed: MotionTier | null = next;
  if (next === currentTier) {
    changed = null;
  }
  return { prev: current, tier: changed };
};

export const useMotionDetection = (mode: MotionMode, active: boolean): MotionDetection => {
  const [detectedTier, setDetectedTier] = useState<MotionTier>('mixed');
  const prevRef = useRef<MotionSnapshot | null>(null);
  const tierRef = useRef<MotionTier>('mixed');

  useEffect(() => {
    if (!active || mode !== 'auto') {
      prevRef.current = null;
      return undefined;
    }

    let disposed = false;
    const poll = async (): Promise<void> => {
      try {
        const result = await sampleMotion(prevRef.current, tierRef.current);
        if (disposed) {
          return undefined;
        }
        prevRef.current = result.prev;
        if (result.tier) {
          tierRef.current = result.tier;
          setDetectedTier(result.tier);
        }
      } catch (error) {
        console.debug('[motion] sample failed:', error);
      }
      if (!disposed) {
        setTimeout(poll, POLL_INTERVAL_MS);
      }
    };
    void poll();

    return () => {
      disposed = true;
    };
  }, [mode, active]);

  if (mode !== 'auto') {
    return { detected: false, motionTier: mode };
  }
  return { detected: true, motionTier: detectedTier };
};
