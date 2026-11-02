import { codecLabel } from '@slopcast/shared-types';
import { useCallback, useEffect, useRef, useState } from 'react';

import { desktopApi } from '../api/desktop';
import { idleTelemetry } from '../components/telemetry/StreamTelemetryBar';
import type { StreamTelemetry } from '../components/telemetry/StreamTelemetryBar';
import type { DesktopCaptureStats, NativeTelemetry } from '../types';

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

interface CaptureStatsPrev {
  dequeued: number;
  at: number;
  init: boolean;
}

interface CaptureSample {
  fps: number | null;
  cursorFrames: number;
  cursorMissing: boolean;
  next: CaptureStatsPrev;
}

interface StatsSnapshot {
  videoMime: string | null;
  videoEnc: string | null;
  audioMime: string | null;
  videoBps: number | null;
  audioBps: number | null;
  fps: number | null;
  captureFps: number | null;
  cursorFrames: number;
  cursorMissing: boolean;
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
  if (t.timestampMs === null || t.videoBytesSent === null) {
    return;
  }
  if (!prev.vInit || t.timestampMs <= prev.vTs) {
    return;
  }
  const dt = (t.timestampMs - prev.vTs) / 1000;
  const db = t.videoBytesSent - prev.vBytes;
  if (db > 0) {
    snap.videoBps = (db * 8) / dt;
  }
  if (t.videoFramesEncoded !== null) {
    const df = t.videoFramesEncoded - prev.vFrames;
    if (df > 0) {
      snap.fps = df / dt;
    }
  }
};

const applyAudioDelta = (snap: StatsSnapshot, t: NativeTelemetry, prev: StatsPrev): void => {
  if (t.timestampMs === null || t.audioBytesSent === null) {
    return;
  }
  if (!prev.aInit || t.timestampMs <= prev.aTs) {
    return;
  }
  const dt = (t.timestampMs - prev.aTs) / 1000;
  const db = t.audioBytesSent - prev.aBytes;
  if (db > 0) {
    snap.audioBps = (db * 8) / dt;
  }
};

const sampleCaptureStats = (stats: DesktopCaptureStats, prev: CaptureStatsPrev): CaptureSample => {
  const nowMs = performance.now();
  let fps: number | null = null;
  if (prev.init && nowMs > prev.at) {
    const dt = (nowMs - prev.at) / 1000;
    const delta = stats.framesDequeued - prev.dequeued;
    if (delta >= 0 && dt > 0) {
      fps = delta / dt;
    }
  }
  return {
    cursorFrames: stats.cursorFrames,
    cursorMissing: stats.cursorMissing,
    fps,
    next: { at: nowMs, dequeued: stats.framesDequeued, init: true },
  };
};

const foldNativeTelemetry = (t: NativeTelemetry, prev: StatsPrev): StatsSnapshot => {
  const snap: StatsSnapshot = {
    audioBps: null,
    audioMime: t.audioCodec,
    captureFps: null,
    cursorFrames: 0,
    cursorMissing: false,
    encHeight: t.videoHeight,
    encWidth: t.videoWidth,
    fps: null,
    packetsLost: (t.videoPacketsLost ?? 0) + (t.audioPacketsLost ?? 0),
    packetsSent: (t.videoPacketsSent ?? 0) + (t.audioPacketsSent ?? 0),
    rttMs: t.rttMs,
    videoBps: null,
    videoEnc: t.encoderImplementation,
    videoMime: t.videoCodec,
  };

  applyVideoDelta(snap, t, prev);
  if (t.videoBytesSent !== null) {
    prev.vInit = true;
    prev.vBytes = t.videoBytesSent;
    prev.vFrames = t.videoFramesEncoded ?? prev.vFrames;
    prev.vTs = t.timestampMs ?? prev.vTs;
  }

  applyAudioDelta(snap, t, prev);
  if (t.audioBytesSent !== null) {
    prev.aInit = true;
    prev.aBytes = t.audioBytesSent;
    prev.aTs = t.timestampMs ?? prev.aTs;
  }

  return snap;
};

const pushSmooth = (buf: number[], value: number): number => {
  buf.push(value);
  if (buf.length > 3) {
    buf.shift();
  }
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
  if (snap.fps !== null) {
    sFps = pushSmooth(fpsBuf, snap.fps);
  }
  let sBr: number | null = null;
  let bitrateHistory = history;
  if (snap.videoBps !== null) {
    const mbps = snap.videoBps / 1_000_000;
    sBr = pushSmooth(brBuf, mbps) * 1_000_000;
    bitrateHistory = [...history, mbps].slice(-STATS_HISTORY_MAX);
  }
  const lossPct = snap.packetsSent > 0 ? (snap.packetsLost / (snap.packetsSent + snap.packetsLost)) * 100 : 0;
  return { bitrateHistory, lossPct, sBr, sFps };
};

const hasSenderTelemetry = (telemetry: NativeTelemetry | null): telemetry is NativeTelemetry => {
  if (!telemetry) {
    return false;
  }
  return telemetry.videoBytesSent !== null || telemetry.audioBytesSent !== null;
};

const telemetryWithoutSender = (
  p: StreamTelemetry,
  inputs: TickInputs,
  spectatorCount: number,
  elapsedMs: number,
): StreamTelemetry => ({
  ...p,
  audioBitrate: null,
  captureFps: null,
  cursorFrames: 0,
  cursorMissing: false,
  elapsedMs,
  hasAudio: inputs.hasAudio,
  height: inputs.height,
  live: true,
  packetLossPct: null,
  spectatorCount,
  targetFrameRate: inputs.targetFrameRate,
  videoBitrate: null,
  width: inputs.width,
});

interface TelemetryUpdateInput {
  readonly previous: StreamTelemetry;
  readonly snap: StatsSnapshot;
  readonly inputs: TickInputs;
  readonly smoothed: Smoothed;
  readonly spectatorCount: number;
  readonly elapsedMs: number;
}

const buildTelemetryUpdate = ({
  previous: p,
  snap,
  inputs,
  smoothed,
  spectatorCount,
  elapsedMs,
}: TelemetryUpdateInput): StreamTelemetry => ({
  audioBitrate: snap.audioBps ?? p.audioBitrate,
  audioCodec: snap.audioMime !== null && snap.audioMime !== '' ? codecLabel(snap.audioMime) : p.audioCodec,
  bitrateHistory: smoothed.bitrateHistory,
  captureFps: snap.captureFps ?? p.captureFps,
  cursorFrames: snap.cursorFrames,
  cursorMissing: snap.cursorMissing,
  elapsedMs,
  frameRate: smoothed.sFps ?? p.frameRate,
  hasAudio: inputs.hasAudio,
  height: snap.encHeight ?? inputs.height,
  live: true,
  packetLossPct: smoothed.lossPct,
  rttMs: snap.rttMs ?? p.rttMs,
  spectatorCount,
  targetFrameRate: inputs.targetFrameRate,
  videoBitrate: smoothed.sBr ?? p.videoBitrate,
  videoCodec: snap.videoMime !== null && snap.videoMime !== '' ? codecLabel(snap.videoMime) : p.videoCodec,
  videoEncoder: snap.videoEnc ?? p.videoEncoder,
  width: snap.encWidth ?? inputs.width,
});

const orDash = (value: number | string | null): string => {
  if (value === null) {
    return '\u2013';
  }
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
  const fpsText = i.sFps === null ? '\u2013' : String(Math.round(i.sFps));
  const capText = i.sCap === null ? '\u2013' : String(Math.round(i.sCap));
  const brText = i.sBr === null ? '\u2013' : (i.sBr / 1_000_000).toFixed(1);
  const rttText = i.rttMs === null ? '\u2013' : String(Math.round(i.rttMs));
  const widthText = orDash(i.encWidth ?? i.width);
  const heightText = orDash(i.encHeight ?? i.height);
  return `[Telemetry] ${orDash(i.videoMime)} ${widthText}\u00d7${heightText} ${fpsText}/${orDash(i.targetFrameRate)}fps cap ${capText}fps ${brText}Mbps loss ${i.lossPct.toFixed(2)}% rtt ${rttText}ms \u00b7 ${i.spectatorCount} spectator(s)`;
};

interface TelemetryLogInput {
  readonly tick: number;
  readonly snap: StatsSnapshot;
  readonly inputs: TickInputs;
  readonly smoothed: Smoothed;
  readonly spectatorCount: number;
}

const maybeLogTelemetry = ({ tick, snap, inputs, smoothed, spectatorCount }: TelemetryLogInput): void => {
  if (tick % 5 !== 0) {
    return;
  }
  console.log(
    telemetryLogLine({
      encHeight: snap.encHeight,
      encWidth: snap.encWidth,
      height: inputs.height,
      lossPct: smoothed.lossPct,
      rttMs: snap.rttMs,
      sBr: smoothed.sBr,
      sCap: snap.captureFps,
      sFps: smoothed.sFps,
      spectatorCount,
      targetFrameRate: inputs.targetFrameRate,
      videoMime: snap.videoMime,
      width: inputs.width,
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

export const useStreamTelemetry = (spectatorCount: number): UseStreamTelemetryReturn => {
  const [telemetry, setTelemetry] = useState<StreamTelemetry>(idleTelemetry());
  const telemetryPollRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const broadcastStartRef = useRef<number | null>(null);
  const statsPrevRef = useRef<StatsPrev>({
    aBytes: 0,
    aInit: false,
    aTs: 0,
    vBytes: 0,
    vFrames: 0,
    vInit: false,
    vTs: 0,
  });
  const capturePrevRef = useRef({ at: 0, dequeued: 0, init: false });
  const bitrateHistoryRef = useRef<number[]>([]);
  const telemetryGenerationRef = useRef(0);
  const spectatorCountRef = useRef(spectatorCount);
  useEffect(() => {
    spectatorCountRef.current = spectatorCount;
  }, [spectatorCount]);

  const resetStatsPrev = useCallback(() => {
    statsPrevRef.current = { aBytes: 0, aInit: false, aTs: 0, vBytes: 0, vFrames: 0, vInit: false, vTs: 0 };
    capturePrevRef.current = { at: 0, dequeued: 0, init: false };
  }, []);

  const stopTelemetryPolling = useCallback(() => {
    telemetryGenerationRef.current += 1;
    if (telemetryPollRef.current !== null) {
      clearInterval(telemetryPollRef.current);
      telemetryPollRef.current = null;
    }
    broadcastStartRef.current = null;
    bitrateHistoryRef.current = [];
    setTelemetry(idleTelemetry());
  }, []);

  const startTelemetryPolling = useCallback((getInputs: () => TickInputs) => {
    if (telemetryPollRef.current !== null) {
      return;
    }
    broadcastStartRef.current = performance.now();

    const generation = telemetryGenerationRef.current + 1;
    const fpsBuf: number[] = [];
    const brBuf: number[] = [];
    let isTicking = false;
    let tick = 0;
    telemetryGenerationRef.current = generation;

    const tickStats = async (): Promise<void> => {
      isTicking = true;
      tick++;
      try {
        const inputs = getInputs();
        const elapsedMs = broadcastStartRef.current === null ? 0 : performance.now() - broadcastStartRef.current;
        const spectatorCountNow = spectatorCountRef.current;
        const [telemetrySample, captureStats] = await Promise.all([
          desktopApi.getNativeTelemetry(),
          desktopApi.getVideoCaptureStats(),
        ]);
        if (generation !== telemetryGenerationRef.current) {
          return;
        }
        if (!hasSenderTelemetry(telemetrySample)) {
          setTelemetry((previous) => telemetryWithoutSender(previous, inputs, spectatorCountNow, elapsedMs));
          return;
        }

        const snap = foldNativeTelemetry(telemetrySample, statsPrevRef.current);
        const capture = sampleCaptureStats(captureStats, capturePrevRef.current);
        snap.captureFps = capture.fps;
        snap.cursorFrames = capture.cursorFrames;
        snap.cursorMissing = capture.cursorMissing;
        capturePrevRef.current = capture.next;
        const smoothed = smoothTelemetry(snap, fpsBuf, brBuf, bitrateHistoryRef.current);
        bitrateHistoryRef.current = smoothed.bitrateHistory;
        setTelemetry((previous) =>
          buildTelemetryUpdate({ elapsedMs, inputs, previous, snap, smoothed, spectatorCount: spectatorCountNow }),
        );
        maybeLogTelemetry({ inputs, snap, smoothed, spectatorCount: spectatorCountNow, tick });
      } catch (error) {
        console.warn('Transient native telemetry failure:', error);
      } finally {
        isTicking = false;
      }
    };

    telemetryPollRef.current = setInterval(() => {
      if (!isTicking) {
        void tickStats();
      }
    }, STATS_POLL_MS);
  }, []);

  return {
    resetStatsPrev,
    setTelemetry,
    startTelemetryPolling,
    stopTelemetryPolling,
    telemetry,
  };
};
