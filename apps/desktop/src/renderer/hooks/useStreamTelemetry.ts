import { codecLabel } from '@slopcast/shared-types';
import type { Room } from 'livekit-client';
import { useCallback, useRef, useState } from 'react';
import { idleTelemetry, type StreamTelemetry } from '../components/telemetry/StreamTelemetryBar';

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

interface RTCStatLike {
  type: string;
  kind?: string;
  timestamp?: number;
  codecId?: string;
  bytesSent?: number;
  packetsSent?: number;
  packetsLost?: number;
  framesSent?: number;
  nominated?: boolean;
  state?: string;
  currentRoundTripTime?: number;
  mimeType?: string;
  implementation?: string;
  frameWidth?: number;
  frameHeight?: number;
}

interface StatsSnapshot {
  videoMime: string | null;
  videoEnc: string | null;
  audioMime: string | null;
  videoBps: number | null;
  audioBps: number | null;
  fps: number | null;
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
  elapsedMs: number;
}

const applyVideoDelta = (snap: StatsSnapshot, report: RTCStatLike, prev: StatsPrev): void => {
  const ts = report.timestamp ?? 0;
  if (!prev.vInit || ts <= prev.vTs) return;
  const dt = (ts - prev.vTs) / 1000;
  const db = (report.bytesSent ?? 0) - prev.vBytes;
  if (db >= 0) snap.videoBps = (db * 8) / dt;
  if (typeof report.framesSent === 'number') {
    const df = report.framesSent - prev.vFrames;
    if (df >= 0) snap.fps = df / dt;
  }
};

const applyAudioDelta = (snap: StatsSnapshot, report: RTCStatLike, prev: StatsPrev): void => {
  const ts = report.timestamp ?? 0;
  if (!prev.aInit || ts <= prev.aTs) return;
  const dt = (ts - prev.aTs) / 1000;
  const db = (report.bytesSent ?? 0) - prev.aBytes;
  if (db >= 0) snap.audioBps = (db * 8) / dt;
};

const codecOf = (report: RTCStatLike, stats: RTCStatsReport): RTCStatLike | undefined => {
  if (!report.codecId) return undefined;
  return stats.get(report.codecId) as RTCStatLike | undefined;
};

const foldVideoReport = (snap: StatsSnapshot, report: RTCStatLike, stats: RTCStatsReport, prev: StatsPrev): void => {
  const codecReport = codecOf(report, stats);
  snap.videoMime = codecReport?.mimeType ?? null;
  snap.videoEnc = codecReport?.implementation ?? null;
  snap.encWidth = report.frameWidth ?? null;
  snap.encHeight = report.frameHeight ?? null;
  snap.packetsSent += report.packetsSent || 0;
  snap.packetsLost += report.packetsLost || 0;
  applyVideoDelta(snap, report, prev);
  prev.vInit = true;
  prev.vBytes = report.bytesSent ?? prev.vBytes;
  prev.vFrames = typeof report.framesSent === 'number' ? report.framesSent : prev.vFrames;
  prev.vTs = report.timestamp || prev.vTs;
};

const foldAudioReport = (snap: StatsSnapshot, report: RTCStatLike, stats: RTCStatsReport, prev: StatsPrev): void => {
  const codecReport = codecOf(report, stats);
  snap.audioMime = codecReport?.mimeType ?? null;
  snap.packetsSent += report.packetsSent || 0;
  snap.packetsLost += report.packetsLost || 0;
  applyAudioDelta(snap, report, prev);
  prev.aInit = true;
  prev.aBytes = report.bytesSent ?? prev.aBytes;
  prev.aTs = report.timestamp || prev.aTs;
};

const foldCandidatePair = (snap: StatsSnapshot, report: RTCStatLike): void => {
  if ((report.nominated || report.state === 'succeeded') && typeof report.currentRoundTripTime === 'number') {
    snap.rttMs = report.currentRoundTripTime * 1000;
  }
};

const foldReport = (snap: StatsSnapshot, report: RTCStatLike, stats: RTCStatsReport, prev: StatsPrev): void => {
  if (report.type === 'outbound-rtp' && report.kind === 'video') {
    foldVideoReport(snap, report, stats, prev);
  } else if (report.type === 'outbound-rtp' && report.kind === 'audio') {
    foldAudioReport(snap, report, stats, prev);
  } else if (report.type === 'candidate-pair') {
    foldCandidatePair(snap, report);
  }
};

const collectStats = async (
  videoSender: RTCRtpSender,
  audioSender: RTCRtpSender | undefined,
  prev: StatsPrev,
): Promise<StatsSnapshot> => {
  const snap: StatsSnapshot = {
    videoMime: null,
    videoEnc: null,
    audioMime: null,
    videoBps: null,
    audioBps: null,
    fps: null,
    packetsSent: 0,
    packetsLost: 0,
    rttMs: null,
    encWidth: null,
    encHeight: null,
  };

  const audioStatsPromise = audioSender ? audioSender.getStats() : Promise.resolve(null);
  const [videoStats, audioStats] = await Promise.all([videoSender.getStats(), audioStatsPromise]);

  for (const stats of [videoStats, audioStats]) {
    if (!stats) continue;
    for (const reportRaw of stats.values()) {
      foldReport(snap, reportRaw as RTCStatLike, stats, prev);
    }
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

const collectTickInputs = (localStream: MediaStream | null, broadcastStart: number | null): TickInputs => {
  const now = performance.now();
  const vTrack = localStream?.getVideoTracks()[0] ?? null;
  const settings = vTrack?.getSettings() ?? null;
  return {
    width: settings?.width ?? null,
    height: settings?.height ?? null,
    targetFrameRate: settings?.frameRate ?? (vTrack ? 60 : null),
    hasAudio: (localStream?.getAudioTracks().length ?? 0) > 0,
    elapsedMs: broadcastStart ? now - broadcastStart : 0,
  };
};

const telemetryWithoutSender = (p: StreamTelemetry, inputs: TickInputs, spectatorCount: number): StreamTelemetry => ({
  ...p,
  live: true,
  width: inputs.width,
  height: inputs.height,
  targetFrameRate: inputs.targetFrameRate,
  hasAudio: inputs.hasAudio,
  elapsedMs: inputs.elapsedMs,
  spectatorCount,
  videoBitrate: null,
  audioBitrate: null,
  packetLossPct: null,
});

const buildTelemetryUpdate = (
  p: StreamTelemetry,
  snap: StatsSnapshot,
  inputs: TickInputs,
  smoothed: Smoothed,
  spectatorCount: number,
): StreamTelemetry => ({
  live: true,
  videoCodec: snap.videoMime ? codecLabel(snap.videoMime) : p.videoCodec,
  videoEncoder: snap.videoEnc ?? p.videoEncoder,
  width: snap.encWidth ?? inputs.width,
  height: snap.encHeight ?? inputs.height,
  targetFrameRate: inputs.targetFrameRate,
  frameRate: smoothed.sFps ?? p.frameRate,
  videoBitrate: smoothed.sBr ?? p.videoBitrate,
  audioCodec: snap.audioMime ? codecLabel(snap.audioMime) : p.audioCodec,
  audioBitrate: snap.audioBps ?? p.audioBitrate,
  hasAudio: inputs.hasAudio,
  packetLossPct: smoothed.lossPct,
  rttMs: snap.rttMs ?? p.rttMs,
  bitrateHistory: smoothed.bitrateHistory,
  elapsedMs: inputs.elapsedMs,
  spectatorCount,
});

const orDash = (value: number | string | null): string => {
  if (value == null) return '–';
  return String(value);
};

interface TelemetryLogInput {
  videoMime: string | null;
  encWidth: number | null;
  width: number | null;
  encHeight: number | null;
  height: number | null;
  sFps: number | null;
  targetFrameRate: number | null;
  sBr: number | null;
  lossPct: number;
  rttMs: number | null;
  spectatorCount: number;
}

const telemetryLogLine = (i: TelemetryLogInput): string => {
  const fpsText = i.sFps != null ? String(Math.round(i.sFps)) : '–';
  const brText = i.sBr != null ? (i.sBr / 1_000_000).toFixed(1) : '–';
  const rttText = i.rttMs != null ? String(Math.round(i.rttMs)) : '–';
  const widthText = orDash(i.encWidth ?? i.width);
  const heightText = orDash(i.encHeight ?? i.height);
  return `[Telemetry] ${orDash(i.videoMime)} ${widthText}×${heightText} ${fpsText}/${orDash(i.targetFrameRate)}fps ${brText}Mbps loss ${i.lossPct.toFixed(2)}% rtt ${rttText}ms · ${i.spectatorCount} spectator(s)`;
};

const videoSenderOf = (room: Room | null): RTCRtpSender | undefined => {
  const videoPub = room?.localParticipant.videoTrackPublications.values().next().value;
  return (videoPub?.track as { sender?: RTCRtpSender } | undefined)?.sender;
};

const audioSenderOf = (room: Room | null): RTCRtpSender | undefined => {
  const audioPub = room?.localParticipant.audioTrackPublications.values().next().value;
  return (audioPub?.track as { sender?: RTCRtpSender } | undefined)?.sender;
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
  startTelemetryPolling: (
    liveKitRoomRef: React.RefObject<Room | null>,
    localStreamRef: React.RefObject<MediaStream | null>,
  ) => void;
  stopTelemetryPolling: () => void;
  resetStatsPrev: () => void;
}

export function useStreamTelemetry(): UseStreamTelemetryReturn {
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
  const bitrateHistoryRef = useRef<number[]>([]);

  const resetStatsPrev = useCallback(() => {
    statsPrevRef.current = { vBytes: 0, vFrames: 0, vTs: 0, vInit: false, aBytes: 0, aTs: 0, aInit: false };
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

  const startTelemetryPolling = useCallback(
    (liveKitRoomRef: React.RefObject<Room | null>, localStreamRef: React.RefObject<MediaStream | null>) => {
      if (telemetryPollRef.current) return;
      broadcastStartRef.current = performance.now();

      const fpsBuf: number[] = [];
      const brBuf: number[] = [];
      let tick = 0;

      const tickStats = async (): Promise<void> => {
        tick++;

        const inputs = collectTickInputs(localStreamRef.current, broadcastStartRef.current);
        const room = liveKitRoomRef.current;
        const spectatorCount = room ? room.remoteParticipants.size : 0;

        const videoSender = videoSenderOf(room);
        if (!videoSender) {
          setTelemetry((p) => telemetryWithoutSender(p, inputs, spectatorCount));
          return;
        }

        try {
          const snap = await collectStats(videoSender, audioSenderOf(room), statsPrevRef.current);
          const smoothed = smoothTelemetry(snap, fpsBuf, brBuf, bitrateHistoryRef.current);
          bitrateHistoryRef.current = smoothed.bitrateHistory;
          setTelemetry((p) => buildTelemetryUpdate(p, snap, inputs, smoothed, spectatorCount));
          maybeLogTelemetry(tick, snap, inputs, smoothed, spectatorCount);
        } catch (err) {
          console.warn('Transient getStats failure:', err);
        }
      };

      telemetryPollRef.current = setInterval(() => {
        void tickStats();
      }, STATS_POLL_MS);
    },
    [],
  );

  return {
    telemetry,
    setTelemetry,
    startTelemetryPolling,
    stopTelemetryPolling,
    resetStatsPrev,
  };
}
