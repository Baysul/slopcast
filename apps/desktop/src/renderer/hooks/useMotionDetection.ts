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
    if (motionRatio >= DYNAMIC_THRESHOLD - HYSTERESIS) return 'dynamic';
    if (motionRatio < STATIC_THRESHOLD) return 'static';
    return 'mixed';
  }
  if (previous === 'static') {
    if (motionRatio <= STATIC_THRESHOLD + HYSTERESIS) return 'static';
    if (motionRatio > DYNAMIC_THRESHOLD) return 'dynamic';
    return 'mixed';
  }
  if (motionRatio < STATIC_THRESHOLD) return 'static';
  if (motionRatio > DYNAMIC_THRESHOLD) return 'dynamic';
  return 'mixed';
};

interface MotionDetection {
  motionTier: MotionTier;
  detected: boolean;
}

interface MotionSnapshot {
  real: number;
  keepalive: number;
}

interface MotionSample {
  tier: MotionTier | null;
  prev: MotionSnapshot;
}

const snapshot = (stats: DesktopCaptureStats): MotionSnapshot => ({
  real: stats.framesPushed,
  keepalive: stats.keepalivePushed,
});

async function sampleMotion(prev: MotionSnapshot | null, currentTier: MotionTier): Promise<MotionSample> {
  const stats = await desktopApi.getVideoCaptureStats();
  const current = snapshot(stats);
  if (!prev) {
    return { tier: null, prev: current };
  }
  const realDelta = current.real - prev.real;
  const keepaliveDelta = current.keepalive - prev.keepalive;
  const total = realDelta + keepaliveDelta;
  if (total <= 0) {
    return { tier: null, prev: current };
  }
  const ratio = realDelta / total;
  const next = classifyMotionTier(ratio, currentTier);
  const changed = next !== currentTier ? next : null;
  return { tier: changed, prev: current };
}

export function useMotionDetection(mode: MotionMode, active: boolean): MotionDetection {
  const [detectedTier, setDetectedTier] = useState<MotionTier>('mixed');
  const prevRef = useRef<{ real: number; keepalive: number } | null>(null);
  const tierRef = useRef<MotionTier>('mixed');

  useEffect(() => {
    if (!active || mode !== 'auto') {
      prevRef.current = null;
      return;
    }

    let disposed = false;
    const poll = async (): Promise<void> => {
      try {
        const result = await sampleMotion(prevRef.current, tierRef.current);
        if (disposed) return;
        prevRef.current = result.prev;
        if (result.tier) {
          tierRef.current = result.tier;
          setDetectedTier(result.tier);
        }
      } catch (error) {
        console.debug('[motion] sample failed:', error);
      }
      if (!disposed) setTimeout(poll, POLL_INTERVAL_MS);
    };
    void poll();

    return () => {
      disposed = true;
    };
  }, [mode, active]);

  if (mode !== 'auto') {
    return { motionTier: mode, detected: false };
  }
  return { motionTier: detectedTier, detected: true };
}
