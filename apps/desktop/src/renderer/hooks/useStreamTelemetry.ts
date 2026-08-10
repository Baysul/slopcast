import { codecLabel } from '@slopcast/shared-types';
import { useCallback, useRef, useState } from 'react';
import { desktopApi } from '../api/desktop';
import { idleTelemetry, type StreamTelemetry } from '../components/telemetry/StreamTelemetryBar';
import type { NativeTelemetry } from '../types';

const STATS_POLL_MS = 1000;
const STATS_HISTORY_MAX = 48;

interface StatsPrev {
  vBytes: number;
  vFrames: number;
  vTs: number;
  vInit: boolean;
  aBytes: number;
  aTs: number;
  aInit: boolean;
}

interface StatsSnapshot {
  videoMime: string | null;
  videoEnc: string | null;
  audioMime: string | null;
  videoBps: number | null;
  audioBps: number | null;
  fps: number | null;
  captureFps: number | null;
  packetsSent: number;
  packetsLost: number;
  rttMs: number | null;
  encWidth: number | null;
  encHeight: number | null;
}

interface TickInputs {
  width: number | null;
  height: number | null;
  targetFrameRate: number | null;
  hasAudio: boolean;
}

const applyVideoDelta = (snap: StatsSnapshot, t: NativeTelemetry, prev: StatsPrev): void => {
  if (t.timestampMs == null || t.videoBytesSent == null) return;
  if (!prev.vInit || t.timestampMs <= prev.vTs) return;
  const dt = (t.timestampMs - prev.vTs) / 1000;
  const db = t.videoBytesSent - prev.vBytes;
  // Strictly-positive deltas only: a zero delta is "no movement this poll",
  // not a measured 0 bps/fps — showing a confident 0 (e.g. "0 fps") during
  // any stall is what made the bar read as broken. Null keeps the last
  // known value on screen.
  if (db > 0) snap.videoBps = (db * 8) / dt;
  if (t.videoFramesEncoded != null) {
    const df = t.videoFramesEncoded - prev.vFrames;
    if (df > 0) snap.fps = df / dt;
  }
};

const applyAudioDelta = (snap: StatsSnapshot, t: NativeTelemetry, prev: StatsPrev): void => {
  if (t.timestampMs == null || t.audioBytesSent == null) return;
  if (!prev.aInit || t.timestampMs <= prev.aTs) return;
  const dt = (t.timestampMs - prev.aTs) / 1000;
  const db = t.audioBytesSent - prev.aBytes;
  if (db > 0) snap.audioBps = (db * 8) / dt;
};

/** Capture-side rate: frames dequeued from the source since the last poll.
 * The capture stats carry no timestamp, so the poll cadence is the clock. */
async function sampleCaptureFps(prev: {
  dequeued: number;
  at: number;
  init: boolean;
}): Promise<{ fps: number | null; next: { dequeued: number; at: number; init: boolean } }> {
  const stats = await desktopApi.getVideoCaptureStats();
  const nowMs = performance.now();
  let fps: number | null = null;
  if (prev.init && nowMs > prev.at) {
    const dt = (nowMs - prev.at) / 1000;
    const delta = stats.framesDequeued - prev.dequeued;
    if (delta >= 0 && dt > 0) fps = delta / dt;
  }
  return {
    fps,
    next: { dequeued: stats.framesDequeued, at: nowMs, init: true },
  };
}

// Native-livekit reports cumulative libwebrtc counters; deltas are computed
// here exactly like the old renderer-side getStats() path did.
const foldNativeTelemetry = (t: NativeTelemetry, prev: StatsPrev): StatsSnapshot => {
  const snap: StatsSnapshot = {
    videoMime: t.videoCodec,
    // The outbound-rtp `encoderImplementation` stat: "VAAPI H264 Encoder"
    // (hardware) vs "OpenH264"/"libvpx" (software) — surfaces whether the
    // hardware encoder is actually in use.
    videoEnc: t.encoderImplementation,
    audioMime: t.audioCodec,
    videoBps: null,
    audioBps: null,
    fps: null,
    captureFps: null,
    packetsSent: (t.videoPacketsSent ?? 0) + (t.audioPacketsSent ?? 0),
    packetsLost: (t.videoPacketsLost ?? 0) + (t.audioPacketsLost ?? 0),
    rttMs: t.rttMs,
    encWidth: t.videoWidth,
    encHeight: t.videoHeight,
  };

  applyVideoDelta(snap, t, prev);
  if (t.videoBytesSent != null) {
    prev.vInit = true;
    prev.vBytes = t.videoBytesSent;
    prev.vFrames = t.videoFramesEncoded ?? prev.vFrames;
    prev.vTs = t.timestampMs ?? prev.vTs;
  }

  applyAudioDelta(snap, t, prev);
  if (t.audioBytesSent != null) {
    prev.aInit = true;
    prev.aBytes = t.audioBytesSent;
    prev.aTs = t.timestampMs ?? prev.aTs;
  }

  return snap;
};

const pushSmooth = (buf: number[], value: number): number => {
  buf.push(value);
  if (buf.length > 3) buf.shift();
  return buf.reduce((a, b) => a + b, 0) / buf.length;
};

interface Smoothed {
  sFps: number | null;
  sBr: number | null;
  lossPct: number;
  bitrateHistory: number[];
}

const smoothTelemetry = (snap: StatsSnapshot, fpsBuf: number[], brBuf: number[], history: number[]): Smoothed => {
  let sFps: number | null = null;
  if (snap.fps != null) {
    sFps = pushSmooth(fpsBuf, snap.fps);
  }
  let sBr: number | null = null;
  let bitrateHistory = history;
  if (snap.videoBps != null) {
    const mbps = snap.videoBps / 1_000_000;
    sBr = pushSmooth(brBuf, mbps) * 1_000_000;
    bitrateHistory = [...history, mbps].slice(-STATS_HISTORY_MAX);
  }
  const lossPct = snap.packetsSent > 0 ? (snap.packetsLost / (snap.packetsSent + snap.packetsLost)) * 100 : 0;
  return { sFps, sBr, lossPct, bitrateHistory };
};

const telemetryWithoutSender = (
  p: StreamTelemetry,
  inputs: TickInputs,
  spectatorCount: number,
  elapsedMs: number,
): StreamTelemetry => ({
  ...p,
  live: true,
  width: inputs.width,
  height: inputs.height,
  targetFrameRate: inputs.targetFrameRate,
  hasAudio: inputs.hasAudio,
  elapsedMs,
  spectatorCount,
  videoBitrate: null,
  audioBitrate: null,
  packetLossPct: null,
  captureFps: null,
});

const buildTelemetryUpdate = (
  p: StreamTelemetry,
  snap: StatsSnapshot,
  inputs: TickInputs,
  smoothed: Smoothed,
  spectatorCount: number,
  elapsedMs: number,
): StreamTelemetry => ({
  live: true,
  videoCodec: snap.videoMime ? codecLabel(snap.videoMime) : p.videoCodec,
  videoEncoder: snap.videoEnc ?? p.videoEncoder,
  width: snap.encWidth ?? inputs.width,
  height: snap.encHeight ?? inputs.height,
  targetFrameRate: inputs.targetFrameRate,
  frameRate: smoothed.sFps ?? p.frameRate,
  videoBitrate: smoothed.sBr ?? p.videoBitrate,
  captureFps: snap.captureFps ?? p.captureFps,
  audioCodec: snap.audioMime ? codecLabel(snap.audioMime) : p.audioCodec,
  audioBitrate: snap.audioBps ?? p.audioBitrate,
  hasAudio: inputs.hasAudio,
  packetLossPct: smoothed.lossPct,
  rttMs: snap.rttMs ?? p.rttMs,
  bitrateHistory: smoothed.bitrateHistory,
  elapsedMs,
  spectatorCount,
});

const orDash = (value: number | string | null): string => {
  if (value == null) return '\u2013';
  return String(value);
};

const telemetryLogLine = (i: {
  videoMime: string | null;
  encWidth: number | null;
  width: number | null;
  encHeight: number | null;
  height: number | null;
  sFps: number | null;
  sCap: number | null;
  targetFrameRate: number | null;
  sBr: number | null;
  lossPct: number;
  rttMs: number | null;
  spectatorCount: number;
}): string => {
  const fpsText = i.sFps != null ? String(Math.round(i.sFps)) : '\u2013';
  const capText = i.sCap != null ? String(Math.round(i.sCap)) : '\u2013';
  const brText = i.sBr != null ? (i.sBr / 1_000_000).toFixed(1) : '\u2013';
  const rttText = i.rttMs != null ? String(Math.round(i.rttMs)) : '\u2013';
  const widthText = orDash(i.encWidth ?? i.width);
  const heightText = orDash(i.encHeight ?? i.height);
  return `[Telemetry] ${orDash(i.videoMime)} ${widthText}\u00d7${heightText} ${fpsText}/${orDash(i.targetFrameRate)}fps cap ${capText}fps ${brText}Mbps loss ${i.lossPct.toFixed(2)}% rtt ${rttText}ms \u00b7 ${i.spectatorCount} spectator(s)`;
};

const maybeLogTelemetry = (
  tick: number,
  snap: StatsSnapshot,
  inputs: TickInputs,
  smoothed: Smoothed,
  spectatorCount: number,
): void => {
  if (tick % 5 !== 0) return;
  console.log(
    telemetryLogLine({
      videoMime: snap.videoMime,
      encWidth: snap.encWidth,
      width: inputs.width,
      encHeight: snap.encHeight,
      height: inputs.height,
      sFps: smoothed.sFps,
      sCap: snap.captureFps,
      targetFrameRate: inputs.targetFrameRate,
      sBr: smoothed.sBr,
      lossPct: smoothed.lossPct,
      rttMs: snap.rttMs,
      spectatorCount,
    }),
  );
};

export interface UseStreamTelemetryReturn {
  telemetry: StreamTelemetry;
  setTelemetry: React.Dispatch<React.SetStateAction<StreamTelemetry>>;
  startTelemetryPolling: (getInputs: () => TickInputs) => void;
  stopTelemetryPolling: () => void;
  resetStatsPrev: () => void;
}

export function useStreamTelemetry(spectatorCount: number): UseStreamTelemetryReturn {
  const [telemetry, setTelemetry] = useState<StreamTelemetry>(idleTelemetry());
  const telemetryPollRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const broadcastStartRef = useRef<number | null>(null);
  const statsPrevRef = useRef<StatsPrev>({
    vBytes: 0,
    vFrames: 0,
    vTs: 0,
    vInit: false,
    aBytes: 0,
    aTs: 0,
    aInit: false,
  });
  const capturePrevRef = useRef({ dequeued: 0, at: 0, init: false });
  const bitrateHistoryRef = useRef<number[]>([]);
  const spectatorCountRef = useRef(spectatorCount);
  spectatorCountRef.current = spectatorCount;

  const resetStatsPrev = useCallback(() => {
    statsPrevRef.current = { vBytes: 0, vFrames: 0, vTs: 0, vInit: false, aBytes: 0, aTs: 0, aInit: false };
    capturePrevRef.current = { dequeued: 0, at: 0, init: false };
  }, []);

  const stopTelemetryPolling = useCallback(() => {
    if (telemetryPollRef.current) {
      clearInterval(telemetryPollRef.current);
      telemetryPollRef.current = null;
    }
    broadcastStartRef.current = null;
    bitrateHistoryRef.current = [];
    setTelemetry(idleTelemetry());
  }, []);

  const startTelemetryPolling = useCallback((getInputs: () => TickInputs) => {
    if (telemetryPollRef.current) return;
    broadcastStartRef.current = performance.now();

    const fpsBuf: number[] = [];
    const brBuf: number[] = [];
    let tick = 0;

    const tickStats = async (): Promise<void> => {
      tick++;

      const inputs = getInputs();
      const elapsedMs = broadcastStartRef.current ? performance.now() - broadcastStartRef.current : 0;
      const spectatorCount = spectatorCountRef.current;

      const t = await desktopApi.getNativeTelemetry();
      if (!t || (t.videoBytesSent == null && t.audioBytesSent == null)) {
        setTelemetry((p) => telemetryWithoutSender(p, inputs, spectatorCount, elapsedMs));
        return;
      }

      try {
        const snap = foldNativeTelemetry(t, statsPrevRef.current);
        const capture = await sampleCaptureFps(capturePrevRef.current);
        snap.captureFps = capture.fps;
        capturePrevRef.current = capture.next;
        const smoothed = smoothTelemetry(snap, fpsBuf, brBuf, bitrateHistoryRef.current);
        bitrateHistoryRef.current = smoothed.bitrateHistory;
        setTelemetry((p) => buildTelemetryUpdate(p, snap, inputs, smoothed, spectatorCount, elapsedMs));
        maybeLogTelemetry(tick, snap, inputs, smoothed, spectatorCount);
      } catch (err) {
        console.warn('Transient native telemetry failure:', err);
      }
    };

    telemetryPollRef.current = setInterval(() => {
      void tickStats();
    }, STATS_POLL_MS);
  }, []);

  return {
    telemetry,
    setTelemetry,
    startTelemetryPolling,
    stopTelemetryPolling,
    resetStatsPrev,
  };
}
