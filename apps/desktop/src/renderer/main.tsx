import { Check, ScreenShare, Users } from 'lucide-react';
import type React from 'react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { Badge } from './components/ui/Badge';
import { primeAudioContext, ToastViewport, useToasts } from './components/ui/Toast';
import './index.css';

// Module-level VAAPI diagnostic — runs before React mounts, no IPC needed.
console.log(
  '[Presenter] Module load — H.264 profiles:',
  RTCRtpSender.getCapabilities('video')
    ?.codecs?.filter((c) => c.mimeType.toUpperCase() === 'VIDEO/H264')
    .map((c) => c.sdpFmtpLine?.match(/profile-level-id=([0-9a-fA-F]{6})/)?.[1])
    .filter(Boolean)
    .join(', ') || 'NONE',
);

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
  frameWidth?: number;
  frameHeight?: number;
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
  const [previewStream, setPreviewStream] = useState<MediaStream | null>(null);
  const [spectatorCount, setSpectatorCount] = useState(0);
  const [captureContext, setCaptureContext] = useState<CaptureContext | null>(null);
  const [autoDetectFailed, setAutoDetectFailed] = useState(false);
  const [telemetry, setTelemetry] = useState<StreamTelemetry>(idleTelemetry());
  const { toasts, push: pushToast, dismiss: dismissToast } = useToasts();

  // ── Stream Settings (user-configurable encoder parameters) ───────────
  const [streamFps, setStreamFps] = useState(60);
  const [bitrateLimit, setBitrateLimit] = useState(20_000_000);
  const [scaleResolutionDownBy, setScaleResolutionDownBy] = useState(1.0);

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
  const prevConnectedRef = useRef(0);
  const streamFpsRef = useRef(60);
  const bitrateLimitRef = useRef(20_000_000);
  const scaleRef = useRef(1.0);

  useEffect(() => {
    isSharingRef.current = isSharing;
  }, [isSharing]);

  useEffect(() => {
    roomCodeRef.current = roomCode;
  }, [roomCode]);

  useEffect(() => {
    streamFpsRef.current = streamFps;
  }, [streamFps]);

  useEffect(() => {
    bitrateLimitRef.current = bitrateLimit;
  }, [bitrateLimit]);

  useEffect(() => {
    scaleRef.current = scaleResolutionDownBy;
  }, [scaleResolutionDownBy]);

  // Push live encoder parameter updates when settings change during an active stream.
  useEffect(() => {
    if (!isSharing) return;
    const fps = streamFps;
    const br = bitrateLimit;
    const scale = scaleResolutionDownBy;
    let cancelled = false;
    const apply = async () => {
      let updated = 0;
      for (const pc of peerConnectionsRef.current.values()) {
        if (cancelled) return;
        if (pc.connectionState !== 'connected') continue;
        const sender = pc.getSenders().find((s) => s.track?.kind === 'video');
        if (!sender) continue;
        try {
          const params = sender.getParameters();
          if (!params.encodings?.length) params.encodings = [{}];
          for (const enc of params.encodings) {
            enc.maxBitrate = br;
            enc.maxFramerate = fps;
            enc.scaleResolutionDownBy = scale;
            enc.priority = 'high';
            enc.networkPriority = 'high';
            enc.active = true;
          }
          await sender.setParameters(params);
          updated++;
        } catch (err) {
          console.warn('[Presenter] live encoder update failed for peer:', err);
        }
      }
      if (updated > 0) {
        console.log(
          `[Presenter] Live encoder update: fps=${fps} bitrate=${(br / 1_000_000).toFixed(0)}Mbps scale=${scale} → ${updated} peer(s)`,
        );
      }
    };
    void apply();
    return () => {
      cancelled = true;
    };
  }, [streamFps, scaleResolutionDownBy, bitrateLimit, isSharing]);

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

      console.log(
        '[Presenter] App start — H.264 profiles in getCapabilities:',
        RTCRtpSender.getCapabilities('video')
          ?.codecs?.filter((c) => c.mimeType.toUpperCase() === 'VIDEO/H264')
          .map((c) => c.sdpFmtpLine?.match(/profile-level-id=([0-9a-fA-F]{6})/)?.[1])
          .filter(Boolean)
          .join(', ') || 'none',
      );
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
      const codecOrder = ['VIDEO/H264'];

      console.log(
        '[Presenter] H.264 profiles in getCapabilities:',
        caps.codecs
          .filter((c) => c.mimeType.toUpperCase() === 'VIDEO/H264')
          .map((c) => c.sdpFmtpLine?.match(/profile-level-id=([0-9a-fA-F]{6})/)?.[1])
          .filter(Boolean)
          .join(', '),
      );

      const H264_HIGH = 0x64;
      const H264_MAIN = 0x4d;
      const H264_BASELINE = 0x42;
      const h264ProfileRank = (fmtp?: string): number => {
        if (!fmtp) return 99;
        const m = fmtp.match(/profile-level-id=([0-9a-fA-F]{6})/);
        if (!m) return 99;
        const profile = parseInt(m[1].slice(0, 2), 16);
        switch (profile) {
          case H264_HIGH:
            return 0;
          case H264_MAIN:
            return 1;
          case H264_BASELINE:
            return 2;
          default:
            return 3;
        }
      };

      const preferred = caps.codecs
        .filter((c) => {
          const mt = c.mimeType.toUpperCase();
          return codecOrder.includes(mt);
        })
        .sort((a, b) => {
          const ia = codecOrder.indexOf(a.mimeType.toUpperCase());
          const ib = codecOrder.indexOf(b.mimeType.toUpperCase());
          const da = ia === -1 ? 99 : ia;
          const db = ib === -1 ? 99 : ib;
          if (da !== db) return da - db;
          if (a.mimeType.toUpperCase() === 'VIDEO/H264') {
            return h264ProfileRank(a.sdpFmtpLine) - h264ProfileRank(b.sdpFmtpLine);
          }
          return 0;
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
        const summary = deduped
          .map((c) => {
            const plid = c.sdpFmtpLine?.match(/profile-level-id=([0-9a-fA-F]{6})/)?.[1];
            return plid ? `${c.mimeType}(${plid})` : c.mimeType;
          })
          .join(' > ');
        console.log('[Presenter] Video codec preference set:', summary);
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

      params.degradationPreference = 'maintain-resolution';

      for (const enc of params.encodings) {
        enc.maxBitrate = bitrateLimitRef.current;
        enc.scaleResolutionDownBy = scaleRef.current;
        enc.maxFramerate = streamFpsRef.current;
        enc.priority = 'high';
        enc.networkPriority = 'high';
        enc.active = true;
      }

      await sender.setParameters(params);
      console.log(
        '[Presenter] Video encoder params:',
        `degradationPreference=${params.degradationPreference}`,
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
        let encWidth: number | null = null;
        let encHeight: number | null = null;

        stats.forEach((reportRaw) => {
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
          width: encWidth ?? width,
          height: encHeight ?? height,
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
            `[Telemetry] ${videoMime ?? '?'} ${encWidth ?? width ?? '?'}×${encHeight ?? height ?? '?'} ${
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
              maxBitrate: bitrateLimitRef.current,
              maxFramerate: streamFpsRef.current,
              scaleResolutionDownBy: scaleRef.current,
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
      return;
    }
    await Promise.all(ids.map((id) => createOfferForSpectator(id)));
  };

  const updateConnectedStatus = () => {
    const total = spectatorIdsRef.current.size;
    if (total === 0) return;
    const connected = Array.from(peerConnectionsRef.current.values()).filter(
      (pc) => pc.iceConnectionState === 'connected' || pc.connectionState === 'connected',
    ).length;
    // Surface a new spectator connection as a transient toast with a chime
    // instead of a persistent line in the status bar.
    if (connected > prevConnectedRef.current) {
      pushToast({
        title: 'Spectator Connected',
        description: `Streaming live — connected to ${connected} of ${total} spectator(s)`,
        variant: 'success',
        icon: <Users className="h-5 w-5" aria-hidden="true" />,
      });
    }
    prevConnectedRef.current = connected;
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
      spectatorIdsRef.current.clear();
      setSpectatorCount(0);
      prevConnectedRef.current = 0;

      // Auto-copy the join link as soon as the room exists.
      void copyText(url).then((ok) => {
        if (ok) {
          setCopied('link');
          setTimeout(() => setCopied(null), 2500);
        }
      });
    } else if (type === 'USER_JOINED') {
      const spectatorId = (payload.participant as { id?: string } | undefined)?.id;
      if (!spectatorId) return;

      spectatorIdsRef.current.add(spectatorId);
      setSpectatorCount(spectatorIdsRef.current.size);

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
    primeAudioContext();
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
      setRoomCode('');
      setShareUrl('');
      spectatorIdsRef.current.clear();
      setSpectatorCount(0);
      prevConnectedRef.current = 0;
    };

    ws.onerror = (err) => {
      console.error('WebSocket error:', err);
      pushToast({
        title: 'Connection error',
        description: 'Lost contact with the signaling server.',
        variant: 'error',
      });
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
      const fps = streamFpsRef.current;
      const stream = await navigator.mediaDevices.getDisplayMedia({
        video: {
          frameRate: { ideal: fps, max: fps },
          width: { ideal: 1920, max: 1920 },
          height: { ideal: 1080, max: 1080 },
        },
        audio: false,
      });
      const track = stream.getVideoTracks()[0];
      if (!track) {
        throw new Error('xdg-desktop-portal granted no video track');
      }
      // 'detail' content hint tells the encoder to prioritise resolution
      // over frame-rate under bandwidth pressure — ideal for screenshare.
      track.contentHint = 'detail';
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
          minFrameRate: streamFpsRef.current,
          maxFrameRate: streamFpsRef.current,
          maxWidth: 1920,
          maxHeight: 1080,
        },
      },
    });
    const track = stream.getVideoTracks()[0];
    track.contentHint = 'detail';
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
    primeAudioContext();
    try {
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
          setAutoDetectFailed(false);
          targetAudioId = -1;
        } else {
          pushToast({
            title: 'No audio detected',
            description: 'Sharing video only. Select an audio app and restart to include audio.',
            variant: 'info',
          });
        }
      } else {
        setAutoDetectFailed(false);
      }

      let audioTrack: MediaStreamTrack | null = null;
      if (targetAudioId !== null) {
        try {
          audioTrack = await captureAudioTrack(targetAudioId);
        } catch (err) {
          console.error('Audio capture failed (continuing video-only):', err);
          pushToast({
            title: 'Audio unavailable',
            description: 'Sharing video only — the selected audio source could not be captured.',
            variant: 'info',
          });
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
      pushToast({ title: 'Screenshare failed to start', description: message, variant: 'error' });
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
    prevConnectedRef.current = 0;
    if (window.electronAPI) {
      await window.electronAPI.stopAudioCapture();
    }
    setIsSharing(false);
    setAudioAppExplicitlySet(false);
    setAutoDetectedApp(null);
    setAutoDetectFailed(false);
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
    } else {
      pushToast({ title: 'Copy failed', description: 'Room link could not be copied.', variant: 'error' });
    }
  };

  const handleCopyCode = async () => {
    if (!roomCode) return;
    const ok = await copyText(roomCode);
    if (ok) {
      flashCopied('code');
    } else {
      pushToast({ title: 'Copy failed', description: 'Room code could not be copied.', variant: 'error' });
    }
  };

  const canStartShare = !!roomCode && !isSharing && (isWayland || !!selectedSourceId);
  const startDisabledReason =
    isSharing || canStartShare
      ? null
      : !roomCode
        ? 'Create a live room to start sharing.'
        : 'Select a window above to start sharing.';

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
                {spectatorCount > 0 && (
                  <span className="hidden sm:inline-flex items-center rounded-full px-2.5 py-1 text-xs font-medium text-gray-400 bg-gray-900/80 border border-gray-800 shrink-0 tabular-nums">
                    {spectatorCount} spectator{spectatorCount === 1 ? '' : 's'}
                  </span>
                )}
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

      {/* ===== Main Content ===== */}
      <main className="flex-1 max-w-5xl mx-auto w-full px-6 py-8 space-y-8">
        {/* Screenshare Preview */}
        <div className="bg-gray-900/80 border border-gray-800 rounded-xl overflow-hidden shadow-2xl">
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
            {isSharing && <StreamTelemetryBar telemetry={telemetry} />}
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
                      <div className="min-w-0 flex-1">
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

                      {isSelected && <Check className="w-4 h-4 shrink-0 text-emerald-300" aria-hidden="true" />}
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
                {captureContext?.de === 'kde' && !autoDetectFailed && (
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
              title={isSharing ? 'Stop the broadcast and disconnect all spectators.' : undefined}
              aria-describedby={startDisabledReason ? 'start-screenshare-hint' : undefined}
              className={`w-full py-3 text-sm font-bold rounded-lg transition-all disabled:opacity-50 disabled:cursor-not-allowed focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight focus-visible:ring-offset-2 focus-visible:ring-offset-background ${
                isSharing
                  ? 'bg-destructive/90 hover:bg-destructive text-white'
                  : 'bg-safelight hover:bg-safelight-hover text-safelight-foreground'
              }`}
            >
              {isSharing ? 'Stop Screenshare' : 'Start Screenshare'}
            </button>
            {startDisabledReason && (
              <p id="start-screenshare-hint" className="text-[11px] text-gray-500 leading-relaxed">
                {startDisabledReason}
              </p>
            )}
          </div>
        </div>

        {/* Stream Settings */}
        <div className="bg-gray-900/80 border border-gray-800 rounded-xl p-6 space-y-5">
          <div>
            <h2 className="text-xs font-semibold uppercase tracking-wider text-gray-400">Stream Settings</h2>
            <p className="text-xs text-gray-500 leading-relaxed mt-1">
              Changes apply in real time — no restart needed.
            </p>
          </div>
          <div className="grid grid-cols-1 sm:grid-cols-3 gap-4">
            {/* Resolution */}
            <div className="space-y-1.5">
              <label
                htmlFor="stream-resolution"
                className="block text-[10px] font-semibold uppercase tracking-[0.05em] text-gray-500"
              >
                Resolution
              </label>
              <select
                id="stream-resolution"
                value={scaleResolutionDownBy}
                onChange={(e) => setScaleResolutionDownBy(Number(e.target.value))}
                className="w-full rounded-lg bg-background/90 border border-gray-800 text-sm text-gray-200 py-2 px-3 focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 cursor-pointer"
              >
                <option value={1.0}>1080p (Full HD)</option>
                <option value={1.5}>720p (HD)</option>
                <option value={2.0}>540p</option>
              </select>
            </div>

            {/* Frame Rate */}
            <div className="space-y-1.5">
              <label
                htmlFor="stream-fps"
                className="block text-[10px] font-semibold uppercase tracking-[0.05em] text-gray-500"
              >
                Frame Rate
              </label>
              <select
                id="stream-fps"
                value={streamFps}
                onChange={(e) => setStreamFps(Number(e.target.value))}
                className="w-full rounded-lg bg-background/90 border border-gray-800 text-sm text-gray-200 py-2 px-3 focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 cursor-pointer"
              >
                <option value={15}>15 fps</option>
                <option value={24}>24 fps</option>
                <option value={30}>30 fps</option>
                <option value={60}>60 fps</option>
              </select>
            </div>

            {/* Bitrate Limit */}
            <div className="space-y-1.5">
              <label
                htmlFor="stream-bitrate"
                className="block text-[10px] font-semibold uppercase tracking-[0.05em] text-gray-500"
              >
                Bitrate Limit
              </label>
              <select
                id="stream-bitrate"
                value={bitrateLimit}
                onChange={(e) => setBitrateLimit(Number(e.target.value))}
                className="w-full rounded-lg bg-background/90 border border-gray-800 text-sm text-gray-200 py-2 px-3 focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 cursor-pointer"
              >
                <option value={1_000_000}>1 Mbps</option>
                <option value={2_000_000}>2 Mbps</option>
                <option value={4_000_000}>4 Mbps</option>
                <option value={6_000_000}>6 Mbps</option>
                <option value={10_000_000}>10 Mbps</option>
                <option value={20_000_000}>20 Mbps</option>
              </select>
            </div>
          </div>
        </div>
      </main>

      <ToastViewport toasts={toasts} onDismiss={dismissToast} />
    </div>
  );
};

const rootEl = document.getElementById('root');
if (!rootEl) throw new Error('Missing #root element');
const root = createRoot(rootEl);
root.render(<PresenterApp />);
