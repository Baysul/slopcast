import { ScreenShare } from 'lucide-react';
import type React from 'react';
import { useCallback, useEffect, useRef, useState } from 'react';
import ReactDOM from 'react-dom/client';
import { Badge } from './components/ui/Badge';
import './index.css';

declare global {
  interface Window {
    electronAPI?: {
      getPlatformInfo: () => Promise<{ platform: string; isWayland: boolean }>;
      getAudioApps: () => Promise<Array<{ id: number; name: string; processId: number }>>;
      startAudioCapture: (targetId: number) => Promise<boolean>;
      stopAudioCapture: () => Promise<boolean>;
      getDesktopSources: () => Promise<Array<{ id: string; name: string; thumbnail: string }>>;
      clipboardWriteText: (text: string) => Promise<boolean>;
      resolveAudioSource: (opts?: {
        sourceId?: string;
      }) => Promise<{ id: number; name: string; processId: number } | null>;
      getCaptureContext: () => Promise<CaptureContext | null>;
    };
  }
}

interface CaptureContext {
  de: 'unknown' | 'kde' | 'gnome';
  mediaName: string | null;
  sourceType: 'monitor' | 'window' | 'unknown';
  videoNodeCount: number;
}

async function copyText(text: string): Promise<boolean> {
  if (!text) return false;
  try {
    if (window.electronAPI?.clipboardWriteText) {
      return await window.electronAPI.clipboardWriteText(text);
    }
    await navigator.clipboard.writeText(text);
    return true;
  } catch (err) {
    console.error('copyText failed:', err);
    return false;
  }
}

interface AudioApp {
  id: number;
  name: string;
  processId: number;
}

interface DesktopSource {
  id: string;
  name: string;
  thumbnail: string;
}

/**
 * Finds the native virtual capture microphone ("Screenshare Window Audio").
 * Chromium filters PipeWire sink-monitor sources out of getUserMedia, so the
 * native layer exposes an Audio/Source/Virtual node instead. Device labels are
 * hidden until microphone access has been granted once, so this unlocks labels
 * on demand.
 */
const findCaptureAudioDevice = async (): Promise<MediaDeviceInfo | null> => {
  let devices = await navigator.mediaDevices.enumerateDevices();
  if (devices.some((d) => d.kind === 'audioinput' && !d.label)) {
    const unlock = await navigator.mediaDevices.getUserMedia({ audio: true });
    for (const t of unlock.getTracks()) {
      t.stop();
    }
    devices = await navigator.mediaDevices.enumerateDevices();
  }
  return (
    devices.find(
      (d) =>
        d.kind === 'audioinput' &&
        (d.label.toLowerCase().includes('screenshare') || d.label.toLowerCase().includes('screenshare-window-audio')),
    ) ?? null
  );
};

// ── Stream Telemetry ──────────────────────────────────────────────────
// Surfaces live broadcast health (codec, resolution, frame rate, bitrate,
// packet loss, audio) to the presenter. Read from RTCPeerConnection.getStats()
// of the representative connected spectator link — codec/fps/bitrate match
// across peers because the same outbound track + encoder parameters are used.

const STATS_POLL_MS = 1000;
const STATS_HISTORY_MAX = 48;

const VIDEO_CODEC_LABEL: Record<string, string> = {
  'VIDEO/H264': 'H.264',
  'VIDEO/H265': 'H.265',
  'VIDEO/VP8': 'VP8',
  'VIDEO/VP9': 'VP9',
  'VIDEO/AV1': 'AV1',
  'AUDIO/OPUS': 'Opus',
  'AUDIO/RED': 'RED',
  'AUDIO/G722': 'G.722',
  'AUDIO/TELEPHONEEVENT': 'DTMF',
};

const codecLabel = (mime: string | null | undefined): string | null => {
  if (!mime) return null;
  const up = mime.toUpperCase();
  return VIDEO_CODEC_LABEL[up] ?? up.replace(/^(VIDEO|AUDIO)\//i, '');
};

interface StreamTelemetry {
  live: boolean;
  updatedAt: number;
  videoCodec: string | null;
  videoEncoder: string | null;
  width: number | null;
  height: number | null;
  frameRate: number | null;
  targetFrameRate: number | null;
  videoBitrate: number | null; // bps, rolling-smoothed for display
  audioCodec: string | null;
  audioBitrate: number | null; // bps
  hasAudio: boolean;
  packetLossPct: number | null;
  roundTripTimeMs: number | null;
  bitrateHistory: number[]; // raw Mbps samples, feeds the sparkline
  elapsedMs: number;
  spectatorCount: number;
}

const idleTelemetry = (): StreamTelemetry => ({
  live: false,
  updatedAt: 0,
  videoCodec: null,
  videoEncoder: null,
  width: null,
  height: null,
  frameRate: null,
  targetFrameRate: null,
  videoBitrate: null,
  audioCodec: null,
  audioBitrate: null,
  hasAudio: false,
  packetLossPct: null,
  roundTripTimeMs: null,
  bitrateHistory: [],
  elapsedMs: 0,
  spectatorCount: 0,
});

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
}

const fmtBitrate = (bps: number | null): string => {
  if (bps == null) return '—';
  if (bps >= 1_000_000) return `${(bps / 1_000_000).toFixed(1)} Mbps`;
  return `${Math.max(1, Math.round(bps / 1000))} kbps`;
};

const fmtLoss = (pct: number | null): string => (pct == null ? '—' : `${pct < 0.1 ? pct.toFixed(2) : pct.toFixed(1)}%`);

const fmtDuration = (ms: number): string => {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const pad = (n: number) => n.toString().padStart(2, '0');
  return `${pad(h)}:${pad(m)}:${pad(s)}`;
};

const Sparkline: React.FC<{ data: number[]; width?: number; height?: number }> = ({
  data,
  width = 88,
  height = 22,
}) => {
  const pts =
    data.length >= 2
      ? (() => {
          const max = Math.max(...data);
          const min = Math.min(...data, 0);
          const span = Math.max(max - min, 0.001);
          const step = width / (data.length - 1);
          return data
            .map((v, i) => {
              const x = i * step;
              const y = height - ((v - min) / span) * (height - 3) - 1.5;
              return `${x.toFixed(1)},${y.toFixed(1)}`;
            })
            .join(' ');
        })()
      : '';
  return (
    <svg
      width={width}
      height={height}
      viewBox={`0 0 ${width} ${height}`}
      className="block text-safelight"
      aria-hidden="true"
      preserveAspectRatio="none"
    >
      {data.length >= 2 && (
        <polyline
          points={pts}
          fill="none"
          stroke="currentColor"
          strokeWidth={1.5}
          strokeLinejoin="round"
          strokeLinecap="round"
          vectorEffect="non-scaling-stroke"
        />
      )}
    </svg>
  );
};

const TelemetryCell: React.FC<{
  label: string;
  value: React.ReactNode;
  sub?: React.ReactNode;
  degrade?: boolean;
}> = ({ label, value, sub, degrade }) => (
  <div className="flex flex-col gap-0.5 min-w-0 shrink-0">
    <span
      className={`text-[9px] font-semibold uppercase tracking-[0.1em] leading-none ${
        degrade ? 'text-destructive/75' : 'text-gray-500'
      }`}
    >
      {label}
    </span>
    <span className="flex items-baseline gap-1.5 leading-none">
      <span
        className={`text-sm font-mono font-semibold tabular-nums leading-none ${
          degrade ? 'text-destructive' : 'text-gray-100'
        }`}
      >
        {value}
      </span>
      {sub != null && <span className="text-[10px] font-mono tabular-nums leading-none text-gray-600">{sub}</span>}
    </span>
  </div>
);

const StreamTelemetryBar: React.FC<{ telemetry: StreamTelemetry }> = ({ telemetry: t }) => {
  const fpsDegrade = t.frameRate != null && t.targetFrameRate != null && t.frameRate < t.targetFrameRate * 0.75;
  const lossDegrade = t.packetLossPct != null && t.packetLossPct > 1;
  const fpsSub = t.targetFrameRate != null ? `/ ${Math.round(t.targetFrameRate)}` : undefined;

  return (
    <div className="pointer-events-none absolute inset-x-0 bottom-0 z-20 select-none">
      <div className="absolute inset-0 bg-gradient-to-t from-black/95 via-black/75 to-transparent" />
      <div className="relative px-4 pt-12 pb-3">
        <div className="flex flex-wrap items-end gap-x-5 gap-y-3">
          {/* On-Air */}
          <div className="flex items-center gap-1.5 shrink-0">
            <span className="w-2 h-2 rounded-full bg-safelight animate-pulse" />
            <span className="text-[10px] font-semibold uppercase tracking-[0.14em] text-safelight">On Air</span>
          </div>

          <TelemetryCell
            label="Codec"
            value={t.videoCodec ?? '—'}
            sub={t.videoEncoder ? `· ${t.videoEncoder}` : undefined}
          />
          <TelemetryCell label="Resolution" value={t.width && t.height ? `${t.width}×${t.height}` : '—'} />
          <TelemetryCell
            label="Frame Rate"
            value={t.frameRate != null ? `${Math.round(t.frameRate)} fps` : '—'}
            sub={fpsSub}
            degrade={fpsDegrade}
          />
          <TelemetryCell label="Bitrate" value={fmtBitrate(t.videoBitrate)} />
          <TelemetryCell label="Loss" value={fmtLoss(t.packetLossPct)} degrade={lossDegrade} />

          {/* Audio */}
          <div className="flex flex-col gap-0.5 shrink-0 min-w-0">
            <span className="text-[9px] font-semibold uppercase tracking-[0.1em] leading-none text-gray-500">
              Audio
            </span>
            {t.hasAudio ? (
              <span className="flex items-baseline gap-1.5 leading-none">
                <span className="text-sm font-mono font-semibold tabular-nums leading-none text-gray-100">
                  {t.audioCodec ?? '—'}
                </span>
                {t.audioBitrate != null && (
                  <span className="text-[10px] font-mono tabular-nums leading-none text-gray-500">
                    {fmtBitrate(t.audioBitrate)}
                  </span>
                )}
              </span>
            ) : (
              <span className="text-sm font-mono font-semibold leading-none text-gray-600">video only</span>
            )}
          </div>

          <div className="grow basis-0 min-w-0" />

          {/* Right: bitrate sparkline + elapsed clock */}
          <div className="flex items-end gap-3 shrink-0">
            <div className="flex flex-col items-end gap-1">
              <Sparkline data={t.bitrateHistory} />
              <span className="text-[9px] uppercase tracking-[0.1em] text-gray-600 leading-none">
                {t.bitrateHistory.length > 0 ? `bitrate · last ${t.bitrateHistory.length}s` : 'awaiting uplink'}
              </span>
            </div>
            <div className="flex flex-col gap-0.5 items-end">
              <span className="text-[9px] font-semibold uppercase tracking-[0.1em] leading-none text-gray-500">
                Elapsed
              </span>
              <span className="text-xs font-mono font-semibold tabular-nums leading-none text-gray-300">
                {fmtDuration(t.elapsedMs)}
              </span>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
};

export const PresenterApp: React.FC = () => {
  const [roomCode, setRoomCode] = useState<string>('');
  const [shareUrl, setShareUrl] = useState<string>('');
  const [isWayland, setIsWayland] = useState<boolean>(false);
  const [audioApps, setAudioApps] = useState<AudioApp[]>([]);
  const [selectedAudioAppId, setSelectedAudioAppId] = useState<number | null>(null);
  const [audioAppExplicitlySet, setAudioAppExplicitlySet] = useState(false);
  const [autoDetectedApp, setAutoDetectedApp] = useState<AudioApp | null>(null);
  const [desktopSources, setDesktopSources] = useState<DesktopSource[]>([]);
  const [selectedSourceId, setSelectedSourceId] = useState<string>('');
  const [isSharing, setIsSharing] = useState<boolean>(false);
  const [copied, setCopied] = useState<'link' | 'code' | null>(null);
  const [statusMsg, setStatusMsg] = useState<string>('Ready to create room');
  const [previewStream, setPreviewStream] = useState<MediaStream | null>(null);
  const [spectatorCount, setSpectatorCount] = useState(0);
  const [captureContext, setCaptureContext] = useState<CaptureContext | null>(null);
  const [autoDetectFailed, setAutoDetectFailed] = useState(false);
  const [telemetry, setTelemetry] = useState<StreamTelemetry>(idleTelemetry());

  const wsRef = useRef<WebSocket | null>(null);
  const peerConnectionsRef = useRef<Map<string, RTCPeerConnection>>(new Map());
  const localStreamRef = useRef<MediaStream | null>(null);
  const isSharingRef = useRef(false);
  const roomCodeRef = useRef('');
  const spectatorIdsRef = useRef<Set<string>>(new Set());
  const pendingCandidatesRef = useRef<Map<string, RTCIceCandidateInit[]>>(new Map());
  const handleSignalingMessageRef = useRef<(msg: unknown) => Promise<void>>(async () => {});
  const previewVideoRef = useRef<HTMLVideoElement | null>(null);
  const telemetryPollRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const broadcastStartRef = useRef<number | null>(null);
  const statsPrevRef = useRef(new WeakMap<RTCPeerConnection, StatsPrev>());
  const bitrateHistoryRef = useRef<number[]>([]);

  useEffect(() => {
    isSharingRef.current = isSharing;
  }, [isSharing]);

  useEffect(() => {
    roomCodeRef.current = roomCode;
  }, [roomCode]);

  // Bind the live capture stream to the local preview <video>.
  useEffect(() => {
    const el = previewVideoRef.current;
    if (!el) return;
    el.srcObject = previewStream;
    if (previewStream) {
      el.play().catch(() => {
        /* autoplay can fail until user gesture; muted should make it ok */
      });
    }
  }, [previewStream]);

  const loadAudioApps = useCallback(async () => {
    if (window.electronAPI) {
      const apps = await window.electronAPI.getAudioApps();
      setAudioApps(apps);
    }
  }, []);

  const loadDesktopSources = useCallback(async () => {
    if (window.electronAPI) {
      const sources = await window.electronAPI.getDesktopSources();
      setDesktopSources(sources);
    }
  }, []);

  useEffect(() => {
    (async () => {
      if (window.electronAPI) {
        const info = await window.electronAPI.getPlatformInfo();
        setIsWayland(info.isWayland);
        if (!info.isWayland) {
          loadDesktopSources();
        }
      }
      loadAudioApps();
    })();

    return () => {
      for (const pc of peerConnectionsRef.current.values()) {
        pc.close();
      }
      peerConnectionsRef.current.clear();
      if (telemetryPollRef.current) {
        clearInterval(telemetryPollRef.current);
        telemetryPollRef.current = null;
      }
      if (wsRef.current) {
        wsRef.current.close();
        wsRef.current = null;
      }
    };
  }, [loadAudioApps, loadDesktopSources]);

  const attemptAutoResolve = async (opts?: { sourceId?: string; nameHint?: string }): Promise<AudioApp | null> => {
    if (!window.electronAPI) return null;

    // Layer 1: IPC (Rust — PID match / PW introspect / Rust name match)
    let app = await window.electronAPI.resolveAudioSource(opts ?? {});

    // Layer 2: renderer-side name matching on the cached audio apps list.
    // Fast fallback when PipeWire is slow to enumerate or the PID/introspect
    // layers returned nothing but we have a credible window-title hint.
    if (!app && opts?.nameHint && audioApps.length > 0) {
      app = findBestAudioMatch(audioApps, opts.nameHint);
    }

    // Layer 3: single audio app — safe to assume it's the target.
    if (!app && audioApps.length === 1) {
      app = audioApps[0];
    }

    if (app) {
      setAutoDetectedApp(app);
      setSelectedAudioAppId(app.id);
      setStatusMsg(`Auto-detected audio source: ${app.name}`);
      return app;
    }
    return null;
  };

  const findBestAudioMatch = (apps: AudioApp[], query: string): AudioApp | null => {
    const q = query.toLowerCase();

    let best = apps.find((a) => a.name.toLowerCase() === q);
    if (best) return best;

    best = apps.find((a) => q.includes(a.name.toLowerCase()));
    if (best) return best;

    best = apps.find((a) => a.name.toLowerCase().includes(q));
    if (best) return best;

    const firstWord = q.split(/\s+/)[0];
    if (firstWord) {
      best = apps.find((a) => a.name.toLowerCase().includes(firstWord) || firstWord.includes(a.name.toLowerCase()));
      if (best) return best;
    }

    return null;
  };

  // ── WebRTC Helper: apply optimal codec preference & encoding params ──
  //
  // 1. Prefer H.264 (maps to hardware encoder on most GPUs) over VP9 over
  //    VP8 (usually software-encoded and more CPU-intensive at high res).
  // 2. Set high-bitrate encoding parameters for low-latency LAN streaming.
  //
  const configureVideoTransceiver = (pc: RTCPeerConnection) => {
    const transceivers = pc.getTransceivers();
    const videoTransceiver = transceivers.find((t) => t.sender?.track?.kind === 'video');
    if (!videoTransceiver) return;

    // ── Codec preference ──────────────────────────────────────────────
    // Hardware-accelerated encode: H.264 (HW) > VP9 (HW/SW) > VP8 (SW).
    const caps = RTCRtpSender.getCapabilities('video');
    if (caps?.codecs?.length) {
      const codecOrder = ['VIDEO/H264', 'VIDEO/VP9', 'VIDEO/VP8'];

      const preferred = caps.codecs
        .filter((c) => {
          const mt = c.mimeType.toUpperCase();
          return codecOrder.includes(mt);
        })
        .sort((a, b) => {
          const ia = codecOrder.indexOf(a.mimeType.toUpperCase());
          const ib = codecOrder.indexOf(b.mimeType.toUpperCase());
          return (ia === -1 ? 99 : ia) - (ib === -1 ? 99 : ib);
        });

      // Deduplicate by MIME type — the browser exposes many profile/level
      // variants of the same codec. Keep only the first (highest-priority)
      // entry for each, so the preference list can actually influence the
      // negotiated codec instead of being flooded with duplicates.
      const seen = new Set<string>();
      const deduped = preferred.filter((c) => {
        const key = c.mimeType.toUpperCase();
        if (seen.has(key)) return false;
        seen.add(key);
        return true;
      });

      try {
        videoTransceiver.setCodecPreferences(deduped);
        console.log('[Presenter] Video codec preference set:', deduped.map((c) => c.mimeType).join(' > '));
      } catch (err) {
        console.warn('[Presenter] setCodecPreferences failed:', err);
      }
    }

    // ── Encoding parameters ───────────────────────────────────────────
    // Set generous bitrate caps for LAN screenshare.  The RtpSender
    // parameters are configured after createOffer/setLocalDescription.
  };

  // ── WebRTC Helper: set video encoding bitrate on the sender ────
  const configureVideoEncoderParams = async (pc: RTCPeerConnection) => {
    const sender = pc.getSenders().find((s) => s.track?.kind === 'video');
    if (!sender) return;

    try {
      const params = sender.getParameters();
      if (!params.encodings || params.encodings.length === 0) {
        params.encodings = [{}];
      }

      for (const enc of params.encodings) {
        enc.maxBitrate = 20_000_000;
        enc.scaleResolutionDownBy = 1.0;
        enc.maxFramerate = 60;
        enc.priority = 'high';
        enc.networkPriority = 'high';
        enc.active = true;
      }

      await sender.setParameters(params);
      console.log(
        '[Presenter] Video encoder params:',
        params.encodings.map(
          (e) => `maxBitrate=${e.maxBitrate} fps=${e.maxFramerate} scale=${e.scaleResolutionDownBy}`,
        ),
      );
    } catch (err) {
      console.warn('[Presenter] setParameters failed:', err);
    }
  };

  // ── WebRTC Helper: log the negotiated video codec ──────────────
  const logNegotiatedCodec = async (pc: RTCPeerConnection, label: string) => {
    try {
      const stats = await pc.getStats();
      stats.forEach((report) => {
        if (report.type === 'outbound-rtp') {
          const out = report as { kind?: string; codecId?: string };
          if (out.kind === 'video' && out.codecId) {
            const codecReport = stats.get(out.codecId);
            if (codecReport) {
              const cr = codecReport as { mimeType?: string; implementation?: string };
              console.log(
                `[Presenter] ${label} negotiated codec:`,
                cr.mimeType,
                `encoder=${cr.implementation || 'unknown'}`,
              );
            }
          }
        }
      });
    } catch (_err) {
      // getStats may fail early — ignore.
    }
  };

  // ── WebRTC Helper: unified telemetry polling ──────────────────
  // Polls the representative connected peer's getStats() once per second,
  // derives codec / resolution / frame-rate / bitrate / packet-loss /
  // audio, refreshes the presenter's telemetry overlay, retains a 48s
  // bitrate history for the sparkline, and logs a compact diagnostic to
  // the console every 5 s. Lifetime is bound to the broadcast (started
  // in handleStartShare, stopped in handleStopShare).
  const startTelemetryPolling = () => {
    if (telemetryPollRef.current) return;
    broadcastStartRef.current = performance.now();

    const fpsBuf: number[] = [];
    const brBuf: number[] = [];
    let tick = 0;

    telemetryPollRef.current = setInterval(async () => {
      tick++;

      // Pick the first connected PC as the representative link. Codec /
      // fps / bitrate are consistent across peers (same track + encoder
      // params), so a single peer is the honest read of encode health.
      const rep = Array.from(peerConnectionsRef.current.values()).find((pc) => pc.connectionState === 'connected');

      const now = performance.now();
      const elapsedMs = broadcastStartRef.current ? now - broadcastStartRef.current : 0;
      const vTrack = localStreamRef.current?.getVideoTracks()[0] ?? null;
      const settings = vTrack?.getSettings() ?? null;
      const width = settings?.width ?? null;
      const height = settings?.height ?? null;
      const targetFrameRate = settings?.frameRate ?? (vTrack ? 60 : null);
      const hasAudio = (localStreamRef.current?.getAudioTracks().length ?? 0) > 0;
      const spectatorCount = spectatorIdsRef.current.size;

      if (!rep) {
        // Broadcasting but no uplink yet — keep capture-derived fields,
        // mark link-dependent metrics as unavailable.
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

      const pc = rep;
      let prev = statsPrevRef.current.get(pc);
      if (!prev) {
        prev = { vBytes: 0, vFrames: 0, vTs: 0, vInit: false, aBytes: 0, aTs: 0, aInit: false };
        statsPrevRef.current.set(pc, prev);
      }

      try {
        const stats = await pc.getStats();
        let videoMime: string | null = null;
        let videoEnc: string | null = null;
        let audioMime: string | null = null;
        let videoBps: number | null = null;
        let audioBps: number | null = null;
        let fps: number | null = null;
        let packetsSent = 0;
        let packetsLost = 0;
        let rttMs: number | null = null;

        stats.forEach((reportRaw) => {
          const report = reportRaw as RTCStatLike;
          if (report.type === 'outbound-rtp') {
            const ts = report.timestamp ?? 0;
            const codecReport = report.codecId ? (stats.get(report.codecId) as RTCStatLike | undefined) : undefined;

            if (report.kind === 'video') {
              videoMime = codecReport?.mimeType ?? null;
              videoEnc = codecReport?.implementation ?? null;
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
            if ((report.nominated || report.state === 'succeeded') && typeof report.currentRoundTripTime === 'number') {
              rttMs = report.currentRoundTripTime * 1000;
            }
          }
        });

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

        setTelemetry((p) => ({
          live: true,
          updatedAt: Date.now(),
          videoCodec: videoMime ? codecLabel(videoMime) : p.videoCodec,
          videoEncoder: videoEnc ?? p.videoEncoder,
          width,
          height,
          targetFrameRate,
          frameRate: sFps ?? p.frameRate,
          videoBitrate: sBr ?? p.videoBitrate,
          audioCodec: audioMime ? codecLabel(audioMime) : p.audioCodec,
          audioBitrate: audioBps ?? p.audioBitrate,
          hasAudio,
          packetLossPct: lossPct,
          roundTripTimeMs: rttMs ?? p.roundTripTimeMs,
          bitrateHistory: bitrateHistoryRef.current,
          elapsedMs,
          spectatorCount,
        }));

        if (tick % 5 === 0) {
          console.log(
            `[Telemetry] ${videoMime ?? '?'} ${width ?? '?'}×${height ?? '?'} ${
              sFps != null ? Math.round(sFps) : '–'
            }/${targetFrameRate ?? '–'}fps ${sBr != null ? (sBr / 1_000_000).toFixed(1) : '–'}Mbps loss ${lossPct.toFixed(2)}% rtt ${
              rttMs != null ? Math.round(rttMs) : '–'
            }ms · ${spectatorCount} spectator(s)`,
          );
        }
      } catch {
        // Transient getStats failures during renegotiation/teardown — ignore.
      }
    }, STATS_POLL_MS);
  };

  const createOfferForSpectator = async (spectatorId: string) => {
    if (!localStreamRef.current || !wsRef.current) {
      console.warn(`[Presenter] Cannot offer to ${spectatorId}: no local stream or ws`);
      return;
    }
    if (wsRef.current.readyState !== WebSocket.OPEN) {
      console.warn(`[Presenter] Cannot offer to ${spectatorId}: ws not open`);
      return;
    }

    // Replace any existing PC for this spectator (re-offer after renegotiation).
    const existing = peerConnectionsRef.current.get(spectatorId);
    if (existing) {
      existing.close();
      peerConnectionsRef.current.delete(spectatorId);
    }

    const pc = new RTCPeerConnection({
      iceServers: [{ urls: 'stun:stun.l.google.com:19302' }, { urls: 'stun:stun1.l.google.com:19302' }],
    });

    peerConnectionsRef.current.set(spectatorId, pc);
    pendingCandidatesRef.current.set(spectatorId, []);

    const stream = localStreamRef.current;
    for (const track of stream.getTracks()) {
      console.log(`[Presenter] addTrack ${track.kind} readyState=${track.readyState} to ${spectatorId}`);
      if (track.kind === 'video') {
        pc.addTransceiver(track, {
          direction: 'sendonly',
          streams: [stream],
          sendEncodings: [
            {
              maxBitrate: 20_000_000,
              maxFramerate: 60,
              scaleResolutionDownBy: 1.0,
              priority: 'high',
              networkPriority: 'high',
              active: true,
            },
          ],
        });
      } else {
        pc.addTrack(track, stream);
      }
    }

    // ── Prefer hardware-accelerated video codec (H.264 > VP9 > VP8) ──
    // Must be called after addTrack creates the transceiver, before createOffer.
    configureVideoTransceiver(pc);

    pc.onicecandidate = (event) => {
      if (event.candidate && wsRef.current?.readyState === WebSocket.OPEN) {
        wsRef.current.send(
          JSON.stringify({
            type: 'WEBRTC_SIGNAL',
            payload: {
              targetId: spectatorId,
              signal: { type: 'candidate', candidate: event.candidate.toJSON() },
            },
          }),
        );
      }
    };

    pc.onconnectionstatechange = () => {
      console.log(`[Presenter] PC ${spectatorId} state:`, pc.connectionState);
      if (pc.connectionState === 'failed' || pc.connectionState === 'closed') {
        peerConnectionsRef.current.delete(spectatorId);
      }
      // Telemetry polling is unified across all connected peers — see startTelemetryPolling.
    };

    pc.oniceconnectionstatechange = () => {
      if (pc.iceConnectionState === 'connected') {
        logNegotiatedCodec(pc, spectatorId);
        updateConnectedStatus();
      } else if (pc.iceConnectionState === 'disconnected' || pc.iceConnectionState === 'failed') {
        updateConnectedStatus();
      }
    };

    try {
      const offer = await pc.createOffer({ offerToReceiveVideo: false, offerToReceiveAudio: false });
      await pc.setLocalDescription(offer);

      // ── Set sender encoding parameters (bitrate, priority) now that the
      // local description is in place and the encoder can be configured.
      await configureVideoEncoderParams(pc);

      const payload = {
        type: 'WEBRTC_SIGNAL',
        payload: {
          targetId: spectatorId,
          signal: { type: 'offer', sdp: offer.sdp },
        },
      };
      wsRef.current.send(JSON.stringify(payload));
      console.log(`[Presenter] Sent offer to spectator ${spectatorId} (sdp ${offer.sdp?.length ?? 0} bytes)`);
    } catch (err) {
      console.error(`[Presenter] Failed to create offer for ${spectatorId}:`, err);
      pc.close();
      peerConnectionsRef.current.delete(spectatorId);
    }
  };

  /** Merge local tracking with server room state so we never miss a spectator. */
  const resolveSpectatorIds = async (hintIds?: string[]): Promise<string[]> => {
    const ids = new Set<string>(spectatorIdsRef.current);
    if (hintIds) {
      for (const id of hintIds) ids.add(id);
    }

    const code = roomCodeRef.current;
    if (code) {
      try {
        const res = await fetch(`http://localhost:3001/api/rooms/${encodeURIComponent(code)}`);
        if (res.ok) {
          const room = await res.json();
          const participants = room.participants || {};
          for (const p of Object.values(participants) as Array<{ id: string; role: string }>) {
            if (p.role === 'spectator' && p.id) {
              ids.add(p.id);
              spectatorIdsRef.current.add(p.id);
            }
          }
        }
      } catch (err) {
        console.warn('[Presenter] Failed to fetch room participants:', err);
      }
    }

    setSpectatorCount(ids.size);
    return Array.from(ids);
  };

  const offerToAllSpectators = async (hintIds?: string[]) => {
    const ids = await resolveSpectatorIds(hintIds);
    console.log(`[Presenter] Offering stream to ${ids.length} spectator(s):`, ids);
    if (ids.length === 0) {
      setStatusMsg('Streaming live — waiting for spectators to join...');
      return;
    }
    setStatusMsg(`Streaming live — connecting ${ids.length} spectator(s)...`);
    await Promise.all(ids.map((id) => createOfferForSpectator(id)));
  };

  const updateConnectedStatus = () => {
    const total = spectatorIdsRef.current.size;
    if (total === 0) return;
    const connected = Array.from(peerConnectionsRef.current.values()).filter(
      (pc) => pc.iceConnectionState === 'connected' || pc.connectionState === 'connected',
    ).length;
    if (connected > 0) {
      setStatusMsg(`Streaming live — connected to ${connected} of ${total} spectator(s)`);
    }
  };

  const handleSignalingMessage = async (msg: unknown) => {
    const { type, payload } = msg as { type: string; payload: Record<string, unknown> };
    console.log('[Presenter] signaling message:', type, payload);

    if (type === 'ROOM_CREATED') {
      const code = payload.code as string;
      const url = (payload.shareUrl as string | undefined) || `http://localhost:3000/room/${code}`;
      roomCodeRef.current = code;
      setRoomCode(code);
      setShareUrl(url);
      setStatusMsg(`Room active: ${code}`);
      spectatorIdsRef.current.clear();
      setSpectatorCount(0);

      // Auto-copy the join link as soon as the room exists.
      void copyText(url).then((ok) => {
        if (ok) {
          setCopied('link');
          setStatusMsg(`Room active: ${code} — link copied to clipboard`);
          setTimeout(() => setCopied(null), 2500);
        }
      });
    } else if (type === 'USER_JOINED') {
      const spectatorId = (payload.participant as { id?: string } | undefined)?.id;
      if (!spectatorId) return;

      spectatorIdsRef.current.add(spectatorId);
      setSpectatorCount(spectatorIdsRef.current.size);
      setStatusMsg(`Spectator ${spectatorId} joined room`);

      // Offer immediately if already streaming (refs avoid stale React state).
      if (isSharingRef.current && localStreamRef.current) {
        void createOfferForSpectator(spectatorId);
      }
    } else if (type === 'USER_LEFT') {
      const userId = payload.userId as string | undefined;
      if (!userId) return;
      spectatorIdsRef.current.delete(userId);
      setSpectatorCount(spectatorIdsRef.current.size);
      const pc = peerConnectionsRef.current.get(userId);
      if (pc) {
        pc.close();
        peerConnectionsRef.current.delete(userId);
      }
      pendingCandidatesRef.current.delete(userId);
      updateConnectedStatus();
    } else if (type === 'PUBLISH_ACK') {
      // Server-authoritative list of spectators currently in the room.
      const spectatorIds = (payload?.spectatorIds as string[] | undefined) || [];
      console.log('[Presenter] PUBLISH_ACK spectators:', spectatorIds);
      for (const id of spectatorIds) spectatorIdsRef.current.add(id);
      setSpectatorCount(spectatorIdsRef.current.size);
      if (isSharingRef.current && localStreamRef.current) {
        void offerToAllSpectators(spectatorIds);
      }
    } else if (type === 'PUBLISH_REJECTED') {
      console.error('[Presenter] PUBLISH_REJECTED:', payload?.reason);
      setStatusMsg(`Publish rejected: ${payload?.reason || 'unknown'}`);
    } else if (type === 'WEBRTC_SIGNAL') {
      const sp = payload as { senderId?: string; signal?: Record<string, unknown> };
      const { senderId, signal } = sp;
      if (!senderId || !signal) return;

      const pc = peerConnectionsRef.current.get(senderId);
      if (!pc) {
        // Queue ICE until offer path creates the PC (should be rare).
        if (signal.candidate || signal.type === 'candidate') {
          const list = pendingCandidatesRef.current.get(senderId) || [];
          list.push((signal.candidate || signal) as unknown as RTCIceCandidateInit);
          pendingCandidatesRef.current.set(senderId, list);
        }
        return;
      }

      try {
        if (signal.type === 'answer') {
          if (pc.signalingState === 'have-local-offer') {
            await pc.setRemoteDescription(new RTCSessionDescription(signal as unknown as RTCSessionDescriptionInit));
            // Re-apply encoder params now that remote answer is known —
            // the negotiated codec may affect encoder configuration.
            await configureVideoEncoderParams(pc);
            const queued = pendingCandidatesRef.current.get(senderId) || [];
            for (const c of queued) {
              await pc.addIceCandidate(new RTCIceCandidate(c));
            }
            pendingCandidatesRef.current.set(senderId, []);
            console.log(`[Presenter] Applied answer from ${senderId}`);
          } else {
            console.warn(`[Presenter] Ignoring answer from ${senderId}; signalingState=${pc.signalingState}`);
          }
        } else if (signal.candidate || signal.type === 'candidate') {
          const candidateInit = (signal.candidate || signal) as unknown as RTCIceCandidateInit;
          if (pc.remoteDescription) {
            await pc.addIceCandidate(new RTCIceCandidate(candidateInit));
          } else {
            const list = pendingCandidatesRef.current.get(senderId) || [];
            list.push(candidateInit);
            pendingCandidatesRef.current.set(senderId, list);
          }
        }
      } catch (err) {
        console.error(`[Presenter] Error handling signal from ${senderId}:`, err);
      }
    }
  };

  // Keep WS handler pointed at the latest closure without re-binding the socket.
  handleSignalingMessageRef.current = handleSignalingMessage;

  const handleCreateRoom = () => {
    if (wsRef.current) {
      wsRef.current.close();
    }

    setAudioAppExplicitlySet(false);
    setAutoDetectedApp(null);
    setSelectedAudioAppId(null);
    setAutoDetectFailed(false);

    const ws = new WebSocket('ws://localhost:3001');
    wsRef.current = ws;

    ws.onopen = () => {
      setStatusMsg('Connected to signaling server');
      ws.send(
        JSON.stringify({
          type: 'CREATE_ROOM',
          payload: { clientOrigin: 'desktop' },
        }),
      );
    };

    ws.onmessage = async (event) => {
      try {
        const msg = JSON.parse(event.data);
        await handleSignalingMessageRef.current(msg);
      } catch (err) {
        console.error('Signaling message parse error:', err);
      }
    };

    ws.onclose = () => {
      setStatusMsg('Disconnected from signaling server');
      setRoomCode('');
      setShareUrl('');
      spectatorIdsRef.current.clear();
      setSpectatorCount(0);
    };

    ws.onerror = (err) => {
      console.error('WebSocket error:', err);
      setStatusMsg('Connection error');
    };
  };

  /**
   * Captures the video track of the window to share. On Wayland the window
   * selection happens in the native xdg-desktop-portal dialog; on X11 the
   * in-app source picker selection is used.
   */
  const captureVideoTrack = async (): Promise<MediaStreamTrack> => {
    if (isWayland) {
      // The main-process displayMediaRequestHandler answers this request;
      // xdg-desktop-portal shows the desktop environment's own window picker.
      const stream = await navigator.mediaDevices.getDisplayMedia({
        video: {
          frameRate: { ideal: 60, max: 60 },
          width: { max: 1920 },
          height: { max: 1080 },
        },
        audio: false,
      });
      const track = stream.getVideoTracks()[0];
      if (!track) {
        throw new Error('xdg-desktop-portal granted no video track');
      }
      // 'motion' content hint tells the encoder to prioritise frame-rate
      // over per-frame perfection — ideal for screenshare.
      track.contentHint = 'motion';
      return track;
    }

    if (!selectedSourceId) {
      throw new Error('No capture source selected');
    }
    const stream = await (
      navigator.mediaDevices as unknown as {
        getUserMedia(constraints: {
          audio: boolean;
          video: { mandatory: Record<string, string | number> };
        }): Promise<MediaStream>;
      }
    ).getUserMedia({
      audio: false,
      video: {
        mandatory: {
          chromeMediaSource: 'desktop',
          chromeMediaSourceId: selectedSourceId,
          minFrameRate: 30,
          maxFrameRate: 60,
        },
      },
    });
    const track = stream.getVideoTracks()[0];
    track.contentHint = 'motion';
    return track;
  };

  /**
   * Starts exclusive native capture of the selected application's audio and
   * returns its audio track from the virtual capture microphone. ONLY the
   * selected application's audio is captured.
   */
  const captureAudioTrack = async (targetId: number): Promise<MediaStreamTrack | null> => {
    const started = await window.electronAPI?.startAudioCapture(targetId);
    if (!started) {
      throw new Error('Native audio capture failed to start');
    }

    // The virtual mic can take a moment to appear in Chromium's device list;
    // poll briefly for it.
    for (let attempt = 0; attempt < 40; attempt++) {
      const device = await findCaptureAudioDevice();
      if (device) {
        const stream = await navigator.mediaDevices.getUserMedia({
          audio: {
            deviceId: { exact: device.deviceId },
            echoCancellation: false,
            noiseSuppression: false,
            autoGainControl: false,
          },
        });
        const track = stream.getAudioTracks()[0];
        if (track) {
          return track;
        }
      }
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
    throw new Error('Virtual capture microphone did not appear as an audio device');
  };

  const handleStartShare = async () => {
    try {
      setStatusMsg('Starting capture...');
      const videoTrack = await captureVideoTrack();

      // Auto-detect audio source.  Uses a local variable to avoid the
      // React async-state pitfall — setSelectedAudioAppId is async but
      // we need the resolved ID synchronously right here.
      let targetAudioId: number | null = selectedAudioAppId;

      if (targetAudioId === null && !audioAppExplicitlySet) {
        // Refresh audio app list so auto-resolve has the freshest PipeWire state.
        await loadAudioApps();

        // On Wayland the video track label is a generic portal identifier, not
        // a window title that can be matched.  Pass no hint so the main process
        // falls through to lastCapturedSourceName (set by displayMediaRequestHandler).
        const app = await attemptAutoResolve(
          isWayland ? {} : { sourceId: selectedSourceId, nameHint: videoTrack.label },
        );
        targetAudioId = app?.id ?? null;
      }

      if (targetAudioId === null) {
        setAutoDetectFailed(true);
        // Query capture context from main process (populated by pw-dump)
        let ctx: CaptureContext | null = null;
        if (isWayland && window.electronAPI?.getCaptureContext) {
          ctx = await window.electronAPI.getCaptureContext();
          setCaptureContext(ctx);
        }

        if (isWayland && ctx?.de === 'kde') {
          // On KDE, fall back to system-wide audio capture (link all audio
          // output nodes to the virtual sink) since window identity is not
          // available in PipeWire streams.
          setStatusMsg('Auto-detected system audio (KDE mode)');
          setAutoDetectFailed(false);
          targetAudioId = -1;
        } else if (isWayland) {
          setStatusMsg(
            'No audio source detected — sharing video only. Select an audio app below and ' +
              'restart the screenshare to include audio.',
          );
        } else {
          setStatusMsg(
            'No audio source detected — sharing video only. Select an audio source from the panel and restart to add audio.',
          );
        }
      } else {
        setAutoDetectFailed(false);
      }

      let audioTrack: MediaStreamTrack | null = null;
      if (targetAudioId !== null) {
        try {
          audioTrack = await captureAudioTrack(targetAudioId);
          if (targetAudioId === -1) {
            setStatusMsg('System audio capture started (KDE fallback)');
          }
        } catch (err) {
          console.error('Audio capture failed (continuing video-only):', err);
          if (targetAudioId === -1) {
            setStatusMsg('System audio unavailable — sharing video only');
          } else {
            setStatusMsg('Selected audio source unavailable — sharing video only');
          }
        }
      }

      const tracks = audioTrack ? [videoTrack, audioTrack] : [videoTrack];
      const stream = new MediaStream(tracks);
      localStreamRef.current = stream;
      setPreviewStream(stream);

      videoTrack.onended = () => {
        handleStopShare();
      };

      setIsSharing(true);
      isSharingRef.current = true;
      setStatusMsg('Screenshare streaming live (window audio only)!');

      // Begin the unified telemetry poll (codec/res/fps/bitrate/loss/audio + sparkline).
      setTelemetry({ ...idleTelemetry(), live: true });
      startTelemetryPolling();

      if (wsRef.current && wsRef.current.readyState === WebSocket.OPEN) {
        wsRef.current.send(
          JSON.stringify({
            type: 'PUBLISH_STREAM',
            payload: { streamId: 'desktop-main-display' },
          }),
        );
      }
    } catch (err: unknown) {
      console.error('Failed to capture screen:', err);
      const message = err instanceof Error ? err.message : 'Unknown capture error';
      setStatusMsg(`Capture error: ${message}`);
      if (window.electronAPI) {
        await window.electronAPI.stopAudioCapture();
      }
    }
  };

  const handleStopShare = async () => {
    const stream = localStreamRef.current;
    if (stream) {
      for (const track of stream.getTracks()) {
        track.stop();
      }
      localStreamRef.current = null;
    }
    setPreviewStream(null);
    for (const pc of peerConnectionsRef.current.values()) {
      pc.close();
    }
    peerConnectionsRef.current.clear();
    if (wsRef.current?.readyState === WebSocket.OPEN) {
      wsRef.current.send(JSON.stringify({ type: 'STOP_STREAM', payload: {} }));
    }
    if (telemetryPollRef.current) {
      clearInterval(telemetryPollRef.current);
      telemetryPollRef.current = null;
    }
    broadcastStartRef.current = null;
    bitrateHistoryRef.current = [];
    setTelemetry(idleTelemetry());
    pendingCandidatesRef.current.clear();
    isSharingRef.current = false;
    if (window.electronAPI) {
      await window.electronAPI.stopAudioCapture();
    }
    setIsSharing(false);
    setAudioAppExplicitlySet(false);
    setAutoDetectedApp(null);
    setAutoDetectFailed(false);
    setStatusMsg('Screenshare stopped');
  };

  const flashCopied = (kind: 'link' | 'code') => {
    setCopied(kind);
    setTimeout(() => setCopied(null), 2000);
  };

  const handleCopyLink = async () => {
    const url = shareUrl || (roomCode ? `http://localhost:3000/room/${roomCode}` : '');
    if (!url) return;
    const ok = await copyText(url);
    if (ok) {
      flashCopied('link');
      setStatusMsg('Room link copied to clipboard');
    } else {
      setStatusMsg('Failed to copy room link');
    }
  };

  const handleCopyCode = async () => {
    if (!roomCode) return;
    const ok = await copyText(roomCode);
    if (ok) {
      flashCopied('code');
      setStatusMsg('Room code copied to clipboard');
    } else {
      setStatusMsg('Failed to copy room code');
    }
  };

  const canStartShare = !!roomCode && !isSharing && (isWayland || !!selectedSourceId);

  return (
    <div className="min-h-screen flex flex-col">
      {/* ===== Sticky Header ===== */}
      <header className="sticky top-0 z-10 border-b border-gray-800 bg-background/80 backdrop-blur-md">
        <div className="max-w-5xl mx-auto px-6 h-14 flex items-center justify-between gap-4">
          <div className="flex items-center gap-3 min-w-0">
            <span className="p-2 bg-secondary rounded-xl text-body-text shrink-0">
              <ScreenShare className="w-5 h-5" aria-hidden="true" />
            </span>
            <h1 className="text-lg font-bold text-gray-100 shrink-0 tracking-tight">ScreenShare</h1>
            <span className="hidden sm:inline-flex text-[10px] bg-indigo-500/20 text-indigo-400 px-2 py-0.5 rounded-full border border-indigo-500/30">
              {isWayland ? 'Wayland' : 'X11'}
            </span>
            {isSharing && (
              <span role="status" aria-live="polite">
                <Badge variant="live">
                  <span className="relative w-1.5 h-1.5 shrink-0" aria-hidden="true">
                    <span className="absolute inset-0 rounded-full bg-safelight animate-ping opacity-75" />
                    <span className="absolute inset-0 rounded-full bg-safelight" />
                  </span>
                  LIVE
                </Badge>
              </span>
            )}
          </div>

          <div className="shrink-0">
            {!roomCode ? (
              <button
                type="button"
                onClick={handleCreateRoom}
                className="px-5 py-2 bg-safelight text-safelight-foreground rounded-lg font-semibold text-sm hover:bg-safelight-hover transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight focus-visible:ring-offset-2 focus-visible:ring-offset-background"
              >
                Create Live Room
              </button>
            ) : (
              <div className="flex items-center gap-2">
                <button
                  type="button"
                  onClick={handleCopyCode}
                  className="flex items-center gap-2 bg-gray-900/80 border border-gray-800 px-3 py-1.5 rounded-lg text-xs focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background"
                >
                  <span className="text-gray-400 font-mono">{roomCode}</span>
                  <span className="bg-gray-800 hover:bg-gray-700 text-gray-200 px-2 py-0.5 rounded border border-gray-700 transition-colors">
                    {copied === 'code' ? 'Copied!' : 'Copy'}
                  </span>
                </button>
                <button
                  type="button"
                  onClick={handleCopyLink}
                  className="bg-safelight text-safelight-foreground px-3 py-1.5 rounded-lg text-xs font-semibold hover:bg-safelight-hover transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight focus-visible:ring-offset-2 focus-visible:ring-offset-background"
                >
                  {copied === 'link' ? 'Link Copied!' : 'Copy Link'}
                </button>
              </div>
            )}
          </div>
        </div>
      </header>

      {/* ===== Status Bar ===== */}
      <div className="border-b border-gray-800/50 bg-card/30">
        <div className="max-w-5xl mx-auto px-6 py-1.5">
          <p className="text-xs text-gray-500 truncate">{statusMsg}</p>
        </div>
      </div>

      {/* ===== Main Content ===== */}
      <main className="flex-1 max-w-5xl mx-auto w-full px-6 py-8 space-y-8">
        {/* Screenshare Preview */}
        <div className="bg-gray-900/80 border border-gray-800 rounded-xl overflow-hidden shadow-2xl">
          <div className="flex items-center justify-between px-5 py-3 border-b border-gray-800">
            <h2 className="text-xs font-semibold uppercase tracking-wider text-gray-400">Screenshare Preview</h2>
            <div className="flex items-center gap-4 text-xs">
              <span className="text-gray-500">
                Spectators: <span className="text-gray-200 font-semibold">{spectatorCount}</span>
              </span>
              <span className={`inline-flex items-center gap-1.5 ${isSharing ? 'text-safelight' : 'text-gray-600'}`}>
                <span className={`w-1.5 h-1.5 rounded-full ${isSharing ? 'bg-safelight' : 'bg-gray-600'}`} />
                {isSharing ? 'Broadcasting' : 'Idle'}
              </span>
            </div>
          </div>
          <div className="relative bg-black aspect-video flex items-center justify-center">
            <video
              ref={previewVideoRef}
              autoPlay
              playsInline
              muted
              className={`w-full h-full object-contain ${isSharing ? 'block' : 'hidden'}`}
            />
            {!isSharing && (
              <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 text-center px-6">
                <p className="text-sm text-gray-400 font-medium">No active screenshare</p>
                <p className="text-xs text-gray-600 max-w-sm">
                  Select a window below and start sharing. Audio is auto-detected.
                </p>
              </div>
            )}
            {isSharing && (
              <>
                <div className="absolute top-3 left-3 pointer-events-none select-none text-[9px] font-semibold uppercase tracking-[0.14em] bg-black/60 text-gray-400 px-2 py-1 rounded-md border border-white/10 backdrop-blur-sm">
                  Local Preview · Muted
                </div>
                <StreamTelemetryBar telemetry={telemetry} />
              </>
            )}
          </div>
        </div>

        {/* Controls Grid */}
        <div className="grid grid-cols-1 md:grid-cols-2 gap-8">
          {/* Window Audio Capture */}
          <div className="bg-gray-900/80 border border-gray-800 rounded-xl p-6 space-y-4">
            <div className="flex items-center justify-between">
              <h2 className="text-xs font-semibold uppercase tracking-wider text-gray-400 flex items-center gap-2">
                Window Audio Capture
                {autoDetectedApp && (
                  <span className="text-[10px] font-normal text-safelight bg-safelight-glow px-2 py-0.5 rounded-full border border-safelight/30">
                    Auto ✓
                  </span>
                )}
              </h2>
              <button
                type="button"
                onClick={loadAudioApps}
                className="text-xs text-gray-500 hover:text-gray-300 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background rounded-md"
              >
                Refresh
              </button>
            </div>

            <p className="text-xs text-gray-500 leading-relaxed">
              Auto-detected from your window selection. Click an app below to override — only that app's audio is
              streamed.
            </p>

            {selectedAudioAppId === null && audioApps.length > 0 && !autoDetectedApp && (
              <div className="bg-background/60 border border-gray-800/60 rounded-lg p-2.5 flex items-center justify-between">
                <span className="text-xs text-gray-500">
                  No audio source selected — click an app below to add audio
                </span>
                <span className="shrink-0 px-2.5 py-1 rounded text-[10px] font-semibold bg-gray-800 text-gray-500">
                  None
                </span>
              </div>
            )}
            <div className="space-y-1.5 max-h-56 overflow-y-auto pr-1">
              {audioApps.length === 0 ? (
                <p className="text-xs text-gray-600 text-center py-6">No active audio applications detected</p>
              ) : (
                audioApps.map((app) => {
                  const isSelected = app.id === selectedAudioAppId;
                  const isAutoDetected = autoDetectedApp?.id === app.id;
                  return (
                    <button
                      key={app.id}
                      type="button"
                      onClick={() => {
                        if (app.id === selectedAudioAppId) {
                          setAudioAppExplicitlySet(false);
                          setSelectedAudioAppId(null);
                          setAutoDetectedApp(null);
                        } else {
                          setAudioAppExplicitlySet(true);
                          setSelectedAudioAppId(app.id);
                          setAutoDetectedApp(null);
                        }
                      }}
                      className={`flex items-center justify-between p-2.5 rounded-lg border text-xs transition-all cursor-pointer text-left w-full focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background ${
                        isSelected
                          ? 'bg-emerald-950/40 border-emerald-500/40 text-emerald-200'
                          : 'bg-background/60 border-gray-800/60 text-gray-400 hover:border-gray-700 hover:text-gray-300'
                      }`}
                    >
                      <div className="min-w-0">
                        <span className="font-semibold block truncate">{app.name}</span>
                        <div className="flex items-center gap-2 mt-0.5">
                          <span className="text-[10px] opacity-60">
                            {app.processId > 0 ? `PID: ${app.processId}` : 'PID: unknown'}
                          </span>
                          {isAutoDetected && (
                            <span className="text-[10px] bg-safelight-glow text-safelight/80 px-1.5 py-0.5 rounded-full">
                              auto
                            </span>
                          )}
                        </div>
                      </div>

                      {isSelected ? (
                        <span className="shrink-0 px-2.5 py-1 rounded text-[10px] font-semibold bg-emerald-600 text-white">
                          {isAutoDetected ? 'Auto' : 'Selected'}
                        </span>
                      ) : (
                        <span className="shrink-0 px-2.5 py-1 rounded text-[10px] font-semibold bg-gray-800 text-gray-500">
                          Select
                        </span>
                      )}
                    </button>
                  );
                })
              )}
            </div>
          </div>

          {/* Screenshare Source */}
          <div className="bg-gray-900/80 border border-gray-800 rounded-xl p-6 space-y-4">
            <h2 className="text-xs font-semibold uppercase tracking-wider text-gray-400">Screenshare Source</h2>

            {isWayland ? (
              <div className="space-y-2 text-xs">
                <p className="text-gray-500 leading-relaxed">
                  The system dialog (xdg-desktop-portal) will let you pick the window to share. Audio is auto-detected
                  via PipeWire introspection.
                </p>
                {captureContext?.de === 'kde' && (
                  <p className="text-amber-300/80 bg-amber-950/30 border border-amber-700/30 rounded-lg p-2.5 leading-relaxed">
                    KDE Plasma detected — window identity is unavailable in PipeWire streams. If auto-detection fails,
                    select an audio app manually.
                  </p>
                )}
              </div>
            ) : (
              <div className="grid grid-cols-2 gap-2 max-h-56 overflow-y-auto pr-1">
                {desktopSources.map((source) => {
                  const isSelected = source.id === selectedSourceId;
                  return (
                    <button
                      key={source.id}
                      type="button"
                      onClick={() => {
                        setSelectedSourceId(source.id);
                        void attemptAutoResolve({ sourceId: source.id, nameHint: source.name });
                      }}
                      className={`p-2 rounded-lg border cursor-pointer transition-all text-xs text-center space-y-1.5 w-full focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background ${
                        isSelected
                          ? 'bg-gray-800/50 border-gray-600 ring-1 ring-gray-600/30'
                          : 'bg-background/60 border-gray-800/60 hover:border-gray-700'
                      }`}
                    >
                      <img src={source.thumbnail} alt={source.name} className="w-full h-20 object-cover rounded" />
                      <span className="block font-medium truncate text-gray-300">{source.name}</span>
                    </button>
                  );
                })}
              </div>
            )}

            {autoDetectFailed && captureContext?.de === 'kde' && (
              <div className="bg-amber-950/40 border border-amber-600/30 rounded-lg p-3 space-y-1">
                <p className="text-xs font-semibold text-amber-300">KDE Audio Auto-Detection Failed</p>
                <p className="text-[11px] text-amber-400/70 leading-relaxed">
                  Select an audio app from the panel above, then stop and restart the screenshare.
                </p>
              </div>
            )}

            <button
              type="button"
              onClick={isSharing ? handleStopShare : handleStartShare}
              disabled={!isSharing && !canStartShare}
              className={`w-full py-3 text-sm font-bold rounded-lg transition-all disabled:opacity-50 disabled:cursor-not-allowed focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight focus-visible:ring-offset-2 focus-visible:ring-offset-background ${
                isSharing
                  ? 'bg-destructive/90 hover:bg-destructive text-white'
                  : 'bg-safelight hover:bg-safelight-hover text-safelight-foreground'
              }`}
            >
              {isSharing ? 'Stop Screenshare' : 'Start Screenshare'}
            </button>
          </div>
        </div>
      </main>
    </div>
  );
};

const rootEl = document.getElementById('root');
if (!rootEl) throw new Error('Missing #root element');
const root = ReactDOM.createRoot(rootEl);
root.render(<PresenterApp />);
