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

      telemetryPollRef.current = setInterval(async () => {
        tick++;

        const now = performance.now();
        const elapsedMs = broadcastStartRef.current ? now - broadcastStartRef.current : 0;
        const vTrack = localStreamRef.current?.getVideoTracks()[0] ?? null;
        const settings = vTrack?.getSettings() ?? null;
        const width = settings?.width ?? null;
        const height = settings?.height ?? null;
        const targetFrameRate = settings?.frameRate ?? (vTrack ? 60 : null);
        const hasAudio = (localStreamRef.current?.getAudioTracks().length ?? 0) > 0;

        const room = liveKitRoomRef.current;
        const spectatorCount = room ? room.remoteParticipants.size : 0;

        const videoPub = room?.localParticipant.videoTrackPublications.values().next().value;
        const videoSender = (videoPub?.track as { sender?: RTCRtpSender } | undefined)?.sender;
        const audioPub = room?.localParticipant.audioTrackPublications.values().next().value;
        const audioSender = (audioPub?.track as { sender?: RTCRtpSender } | undefined)?.sender;

        if (!videoSender) {
          setTelemetry((p) => ({
            ...p,
            live: true,
            updatedAt: Date.now(),
            width,
            height,
            targetFrameRate,
            hasAudio,
            elapsedMs,
            spectatorCount,
            videoBitrate: null,
            audioBitrate: null,
            packetLossPct: null,
            roundTripTimeMs: null,
          }));
          return;
        }

        const prev = statsPrevRef.current;

        try {
          let audioStatsPromise: Promise<RTCStatsReport | null> = Promise.resolve(null);
          if (audioSender) {
            audioStatsPromise = audioSender.getStats();
          }
          const [videoStats, audioStats] = await Promise.all([videoSender.getStats(), audioStatsPromise]);
          let videoMime: string | null = null;
          let videoEnc: string | null = null;
          let audioMime: string | null = null;
          let videoBps: number | null = null;
          let audioBps: number | null = null;
          let fps: number | null = null;
          let packetsSent = 0;
          let packetsLost = 0;
          let rttMs: number | null = null;
          let encWidth: number | null = null;
          let encHeight: number | null = null;

          for (const stats of [videoStats, audioStats]) {
            if (!stats) continue;
            for (const reportRaw of stats.values()) {
              const report = reportRaw as RTCStatLike;
              if (report.type === 'outbound-rtp') {
                const ts = report.timestamp ?? 0;
                const codecReport = report.codecId ? (stats.get(report.codecId) as RTCStatLike | undefined) : undefined;

                if (report.kind === 'video') {
                  videoMime = codecReport?.mimeType ?? null;
                  videoEnc = codecReport?.implementation ?? null;
                  encWidth = report.frameWidth ?? null;
                  encHeight = report.frameHeight ?? null;
                  packetsSent += report.packetsSent || 0;
                  packetsLost += report.packetsLost || 0;
                  if (prev.vInit && ts > prev.vTs) {
                    const dt = (ts - prev.vTs) / 1000;
                    const db = (report.bytesSent ?? 0) - prev.vBytes;
                    if (db >= 0) videoBps = (db * 8) / dt;
                    if (typeof report.framesSent === 'number') {
                      const df = report.framesSent - prev.vFrames;
                      if (df >= 0) fps = df / dt;
                    }
                  }
                  prev.vInit = true;
                  prev.vBytes = report.bytesSent ?? prev.vBytes;
                  prev.vFrames = typeof report.framesSent === 'number' ? report.framesSent : prev.vFrames;
                  prev.vTs = ts || prev.vTs;
                } else if (report.kind === 'audio') {
                  audioMime = codecReport?.mimeType ?? null;
                  packetsSent += report.packetsSent || 0;
                  packetsLost += report.packetsLost || 0;
                  if (prev.aInit && ts > prev.aTs) {
                    const dt = (ts - prev.aTs) / 1000;
                    const db = (report.bytesSent ?? 0) - prev.aBytes;
                    if (db >= 0) audioBps = (db * 8) / dt;
                  }
                  prev.aInit = true;
                  prev.aBytes = report.bytesSent ?? prev.aBytes;
                  prev.aTs = ts || prev.aTs;
                }
              } else if (report.type === 'candidate-pair') {
                if (
                  (report.nominated || report.state === 'succeeded') &&
                  typeof report.currentRoundTripTime === 'number'
                ) {
                  rttMs = report.currentRoundTripTime * 1000;
                }
              }
            }
          }

          if (fps != null) {
            fpsBuf.push(fps);
            if (fpsBuf.length > 3) fpsBuf.shift();
          }
          if (videoBps != null) {
            const mbps = videoBps / 1_000_000;
            brBuf.push(mbps);
            if (brBuf.length > 3) brBuf.shift();
            bitrateHistoryRef.current = [...bitrateHistoryRef.current, mbps].slice(-STATS_HISTORY_MAX);
          }
          const sFps = fpsBuf.length ? fpsBuf.reduce((a, b) => a + b, 0) / fpsBuf.length : null;
          const sBr = brBuf.length ? (brBuf.reduce((a, b) => a + b, 0) / brBuf.length) * 1_000_000 : null;
          const lossPct = packetsSent > 0 ? (packetsLost / (packetsSent + packetsLost)) * 100 : 0;

          let videoCodecLabel: string | null = null;
          if (videoMime) {
            videoCodecLabel = codecLabel(videoMime);
          }
          let audioCodecLabel: string | null = null;
          if (audioMime) {
            audioCodecLabel = codecLabel(audioMime);
          }

          setTelemetry((p) => ({
            live: true,
            videoCodec: videoCodecLabel ?? p.videoCodec,
            videoEncoder: videoEnc ?? p.videoEncoder,
            width: encWidth ?? width,
            height: encHeight ?? height,
            targetFrameRate,
            frameRate: sFps ?? p.frameRate,
            videoBitrate: sBr ?? p.videoBitrate,
            audioCodec: audioCodecLabel ?? p.audioCodec,
            audioBitrate: audioBps ?? p.audioBitrate,
            hasAudio,
            packetLossPct: lossPct,
            rttMs: rttMs ?? p.rttMs,
            bitrateHistory: bitrateHistoryRef.current,
            elapsedMs,
            spectatorCount,
          }));

          if (tick % 5 === 0) {
            let fpsText = '–';
            if (sFps != null) {
              fpsText = String(Math.round(sFps));
            }
            let brText = '–';
            if (sBr != null) {
              brText = (sBr / 1_000_000).toFixed(1);
            }
            let rttText = '–';
            if (rttMs != null) {
              rttText = String(Math.round(rttMs));
            }
            console.log(
              `[Telemetry] ${videoMime ?? '?'} ${encWidth ?? width ?? '?'}×${encHeight ?? height ?? '?'} ${fpsText}/${targetFrameRate ?? '–'}fps ${brText}Mbps loss ${lossPct.toFixed(2)}% rtt ${rttText}ms · ${spectatorCount} spectator(s)`,
            );
          }
        } catch (err) {
          console.warn('Transient getStats failure:', err);
        }
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
