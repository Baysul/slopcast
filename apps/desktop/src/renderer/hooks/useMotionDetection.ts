import { useEffect, useRef, useState } from 'react';
import { desktopApi } from '../api/desktop';
import type { DesktopCaptureStats } from '../types';
import type { MotionMode, MotionTier } from '../utils/bitrate';

// Auto-detect content motion from the capture engine's keepalive-vs-real
// frame counters. When the screen is static the capture loop re-submits
// keepalive frames (the same pixels re-encoded); when the screen is moving it
// delivers real frames. The ratio of real frames to total deliveries is the
// motion signal.

const POLL_INTERVAL_MS = 2000;
// Hysteresis bounds keep the tier stable: a tier change needs the motion
// ratio to cross the boundary by a margin, so flicker between tiers on
// borderline content (scrolling text, cursor movement) never causes a
// cascade of encoder restarts.
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
  /** Effective tier: the manual override, or the auto-detected tier. */
  motionTier: MotionTier;
  /** Whether the tier was auto-detected (true while mode is 'auto'). */
  detected: boolean;
}

const snapshot = (stats: DesktopCaptureStats): { real: number; keepalive: number } => ({
  real: stats.framesPushed,
  keepalive: stats.keepalivePushed,
});

// One poll step: read the counters, diff against the previous sample, and
// classify the interval. Returns the new tier only when it changed (or null),
// and records the current sample into `prev` for the next poll.
async function sampleMotion(
  prev: { real: number; keepalive: number } | null,
  currentTier: MotionTier,
): Promise<{ tier: MotionTier | null; prev: { real: number; keepalive: number } }> {
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

/**
 * Resolves the motion tier while streaming. When `mode` is a manual tier it is
 * returned as-is. When `mode` is `auto`, the capture stats are polled on a
 * light 2 s cadence and the keepalive/real-frame ratio is classified with
 * hysteresis — an IPC read of atomic counters, so it never blocks or contends
 * with the encode path.
 */
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
      } catch {
        // Transient IPC failure; the next poll self-heals. Detection is
        // best-effort and must never surface an error to the user.
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
