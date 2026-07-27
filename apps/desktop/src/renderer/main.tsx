import type { AudioApp, AudioAppLevel } from '@slopcast/shared-types';
import { codecLabel, fmtBitrate, fmtLoss } from '@slopcast/shared-types';
import { Room, RoomEvent, Track } from 'livekit-client';
import { Check, ChevronDown, ScreenShare } from 'lucide-react';
import type React from 'react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { AudioLevelMeter } from './components/ui/AudioLevelMeter';
import { Badge } from './components/ui/Badge';
import { primeAudioContext, ToastViewport, useToasts } from './components/ui/Toast';
import './index.css';

declare global {
  interface Window {
    electronAPI?: {
      getAppConfig: () => Promise<{ apiEndpoint: string; livekitUrl: string }>;
      getPlatformInfo: () => Promise<{ platform: string; isWayland: boolean }>;
      getAudioApps: () => Promise<AudioApp[]>;
      startAudioCapture: (targetId: number) => Promise<boolean>;
      stopAudioCapture: () => Promise<boolean>;
      switchAudioCapture: (targetId: number) => Promise<boolean>;
      startAudioMetering: () => Promise<boolean>;
      stopAudioMetering: () => Promise<boolean>;
      getAudioLevels: () => Promise<AudioAppLevel[]>;
      getDesktopSources: () => Promise<Array<{ id: string; name: string; thumbnail: string }>>;
      clipboardWriteText: (text: string) => Promise<boolean>;
      resolveAudioSource: (opts?: { sourceId?: string }) => Promise<AudioApp | null>;
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

interface AudioAppGroup {
  representative: AudioApp;
  members: AudioApp[];
}

// All streams belonging to the same PipeWire client (or same app name when no
// client id is available) are collapsed into a single picker row. This gives
// predictable per-application audio capture on every platform — Windows and
// macOS also target apps, not individual streams. MPRIS now-playing titles
// (where available) or the first member's PipeWire window title is shown as
// the row's subtitle instead of a process ID.
function groupAudioApps(apps: AudioApp[]): AudioAppGroup[] {
  const groups: AudioAppGroup[] = [];
  const identityMap = new Map<string, AudioAppGroup>();
  for (const app of apps) {
    const key = app.clientId != null && app.clientId > 0 ? `c:${app.clientId}` : `n:${app.name.toLowerCase()}`;
    const existing = identityMap.get(key);
    if (existing) {
      existing.members.push(app);
      continue;
    }
    const group: AudioAppGroup = { representative: app, members: [app] };
    groups.push(group);
    identityMap.set(key, group);
  }
  return groups;
}

interface DesktopSource {
  id: string;
  name: string;
  thumbnail: string;
}

// Device labels are hidden until mic access is granted once, so we unlock them.
let audioDevicesLogged = false;

const findCaptureAudioDevice = async (): Promise<MediaDeviceInfo | null> => {
  const devices = await navigator.mediaDevices.enumerateDevices();

  if (!audioDevicesLogged) {
    audioDevicesLogged = true;
    const allInputs = devices.filter((d) => d.kind === 'audioinput');
    console.log(
      '[findCaptureAudioDevice] all audioinput devices:',
      allInputs.map((d) => `${d.deviceId.substring(0, 8)}… "${d.label}" group=${d.groupId.substring(0, 8)}…`),
    );
  }

  // The native layer names the virtual source "Slopcast-Window-Audio"; Chromium
  // surfaces the PipeWire node description as the device label.
  const target = devices.find((d) => d.kind === 'audioinput' && d.label.toLowerCase().includes('slopcast'));
  if (!target) return null;

  console.log(
    `[findCaptureAudioDevice] found: id=${target.deviceId.substring(0, 8)}… label="${target.label}" group=${target.groupId.substring(0, 8)}…`,
  );
  return target;
};

// Read from RTCPeerConnection.getStats() of the published outbound track.

const STATS_POLL_MS = 1000;
const STATS_HISTORY_MAX = 48;

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
  let points = '';
  if (data.length >= 2) {
    const max = Math.max(...data);
    const min = Math.min(...data, 0);
    const span = Math.max(max - min, 0.001);
    const step = width / (data.length - 1);
    points = data
      .map((v, i) => {
        const x = i * step;
        const y = height - ((v - min) / span) * (height - 3) - 1.5;
        return `${x.toFixed(1)},${y.toFixed(1)}`;
      })
      .join(' ');
  }
  return (
    <svg
      width={width}
      height={height}
      viewBox={`0 0 ${width} ${height}`}
      className="block text-safelight"
      aria-hidden="true"
      preserveAspectRatio="none"
    >
      {points && (
        <polyline
          points={points}
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
  const [apiEndpoint, setApiEndpoint] = useState<string>('http://localhost:3001');
  const [livekitUrl, setLivekitUrl] = useState<string>('');
  const [isWayland, setIsWayland] = useState<boolean>(false);
  const [audioApps, setAudioApps] = useState<AudioApp[]>([]);
  const [audioLevels, setAudioLevels] = useState<ReadonlyMap<number, number>>(new Map());
  const audioAppGroups = useMemo(() => groupAudioApps(audioApps), [audioApps]);
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
  const [streamSettingsOpen, setStreamSettingsOpen] = useState(false);
  const [streamFps, setStreamFps] = useState(60);
  const [bitrateLimit, setBitrateLimit] = useState(20_000_000);
  const [scaleResolutionDownBy, setScaleResolutionDownBy] = useState(1.0);

  const liveKitRoomRef = useRef<Room | null>(null);
  const localStreamRef = useRef<MediaStream | null>(null);
  const isSharingRef = useRef(false);
  const roomCodeRef = useRef('');
  const previewVideoRef = useRef<HTMLVideoElement | null>(null);
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
  const streamFpsRef = useRef(60);
  const bitrateLimitRef = useRef(20_000_000);
  const scaleRef = useRef(1.0);
  const audioAppIdRef = useRef<number | null>(null);

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

  useEffect(() => {
    audioAppIdRef.current = selectedAudioAppId;
  }, [selectedAudioAppId]);

  const captureAudioTrack = useCallback(async (targetId: number): Promise<MediaStreamTrack | null> => {
    const started = await window.electronAPI?.startAudioCapture(targetId);
    if (!started) {
      throw new Error('Native audio capture failed to start');
    }

    const unlock = await navigator.mediaDevices.getUserMedia({ audio: true }).catch(() => null);
    for (const t of unlock?.getTracks() ?? []) {
      t.stop();
    }

    for (let attempt = 0; attempt < 20; attempt++) {
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
    throw new Error('Virtual capture source did not appear as an audio input device');
  }, []);

  const replaceAudioTrack = useCallback(async (targetId: number): Promise<void> => {
    const room = liveKitRoomRef.current;
    if (!room) return;

    // Tell Rust to switch which application's audio is linked into the
    // virtual capture node. The node itself stays alive, the existing
    // MediaStreamTrack continues producing audio — the content seamlessly
    // changes to the new target's audio.
    const switched = await window.electronAPI?.switchAudioCapture(targetId);
    if (!switched) {
      throw new Error('Native audio target switch failed');
    }
    audioAppIdRef.current = targetId;
  }, []);

  // Real-time audio source switching while sharing.
  useEffect(() => {
    if (!isSharing) return;
    if (selectedAudioAppId === null) return;

    const prevId = audioAppIdRef.current;
    if (prevId === selectedAudioAppId) return;
    if (prevId == null && selectedAudioAppId != null) {
      audioAppIdRef.current = selectedAudioAppId;
      return;
    }

    const switchAudio = async () => {
      try {
        await replaceAudioTrack(selectedAudioAppId);
        setSelectedAudioAppId(selectedAudioAppId);
        setAutoDetectedApp(null);
        setAudioAppExplicitlySet(true);
      } catch (err) {
        console.error('[Presenter] audio switch failed:', err);
        pushToast({
          title: 'Audio switch failed',
          description: 'Could not switch to the selected audio source.',
          variant: 'error',
        });
      }
    };
    void switchAudio();
  }, [selectedAudioAppId, isSharing, replaceAudioTrack, pushToast]);

  // Push live encoder parameter updates via the published track's sender.
  useEffect(() => {
    if (!isSharing) return;
    const fps = streamFps;
    const br = bitrateLimit;
    const scale = scaleResolutionDownBy;
    const update = async () => {
      const room = liveKitRoomRef.current;
      if (!room) return;
      const pub = room.localParticipant.videoTrackPublications.values().next().value;
      const sender = (pub?.track as { sender?: RTCRtpSender } | undefined)?.sender;
      if (!sender) return;
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
        console.log(
          `[Presenter] Live encoder update: fps=${fps} bitrate=${(br / 1_000_000).toFixed(0)}Mbps scale=${scale}`,
        );

        // Re-apply audio if it was dropped during renegotiation.
        const currentId = audioAppIdRef.current;
        if (currentId != null) {
          const hasAudio = (localStreamRef.current?.getAudioTracks().length ?? 0) > 0;
          if (!hasAudio) {
            console.log('[Presenter] Audio track lost after settings change, re-applying...');
            try {
              await replaceAudioTrack(currentId);
            } catch (err) {
              console.warn('[Presenter] audio re-apply failed:', err);
            }
          }
        }
      } catch (err) {
        console.warn('[Presenter] live encoder update failed:', err);
      }
    };
    void update();
  }, [streamFps, scaleResolutionDownBy, bitrateLimit, isSharing, replaceAudioTrack]);

  // Bind the live capture stream to the local preview <video>.
  useEffect(() => {
    const el = previewVideoRef.current;
    if (!el) return;
    el.srcObject = previewStream;
    if (previewStream) {
      el.play().catch(() => console.warn('Video autoplay blocked until user gesture'));
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

        const config = await window.electronAPI.getAppConfig();
        if (config.apiEndpoint) setApiEndpoint(config.apiEndpoint);
        if (config.livekitUrl) setLivekitUrl(config.livekitUrl);
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
      liveKitRoomRef.current?.disconnect();
      liveKitRoomRef.current = null;
      if (telemetryPollRef.current) {
        clearInterval(telemetryPollRef.current);
        telemetryPollRef.current = null;
      }
    };
  }, [loadAudioApps, loadDesktopSources]);

  // Per-app audio metering: native PipeWire meter streams report a peak level
  // per audio node; decay applied here so bars fall smoothly between polls.
  useEffect(() => {
    const api = window.electronAPI;
    if (!api?.startAudioMetering) return;

    let cancelled = false;
    let interval: ReturnType<typeof setInterval> | null = null;

    void api.startAudioMetering().then((started) => {
      if (!started || cancelled) return;
      interval = setInterval(() => {
        void api.getAudioLevels().then((levels) => {
          if (cancelled) return;
          setAudioLevels((prev) => {
            const next = new Map<number, number>();
            for (const { id, level } of levels) {
              next.set(id, Math.max(level, (prev.get(id) ?? 0) * 0.72));
            }
            return next;
          });
        });
      }, 150);
    });

    return () => {
      cancelled = true;
      if (interval) clearInterval(interval);
      void api.stopAudioMetering().catch(() => {});
    };
  }, []);

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

    // Layer 4: monitor/display source — fall back to system audio.
    if (!app && window.electronAPI?.getCaptureContext) {
      const ctx = await window.electronAPI.getCaptureContext();
      setCaptureContext(ctx);
      if (ctx?.sourceType === 'monitor') {
        app = { id: -1, name: 'System Audio', processId: 0 };
      }
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

  // Polls the published track sender's getStats() once per second.
  // Started in handleStartShare, stopped in handleStopShare.
  const startTelemetryPolling = () => {
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

      // Get senders from published tracks.
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
        const [videoStats, audioStats] = await Promise.all([
          videoSender.getStats(),
          audioSender ? audioSender.getStats() : Promise.resolve(null),
        ]);
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
      } catch (err) {
        console.warn('Transient getStats failure:', err);
      }
    }, STATS_POLL_MS);
  };

  const handleCreateRoom = async () => {
    primeAudioContext();

    const oldRoom = liveKitRoomRef.current;
    if (oldRoom) {
      oldRoom.removeAllListeners();
      oldRoom.disconnect();
      liveKitRoomRef.current = null;
    }

    setAudioAppExplicitlySet(false);
    setAutoDetectedApp(null);
    setSelectedAudioAppId(null);
    setAutoDetectFailed(false);

    try {
      const res = await fetch(`${apiEndpoint}/api/rooms`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
      });
      if (!res.ok) {
        const err = await res.json().catch(() => ({ error: 'Unknown server error' }));
        throw new Error(err.error || `Server returned ${res.status}`);
      }
      const room = await res.json();
      const code = room.code as string;
      const url = room.shareUrl as string;
      const token = room.token as string;
      const apiLivekitUrl = room.livekitUrl as string;
      const resolvedLivekitUrl = livekitUrl || apiLivekitUrl;

      roomCodeRef.current = code;
      setRoomCode(code);
      setShareUrl(url);
      setSpectatorCount(0);

      void copyText(url).then((ok) => {
        if (ok) {
          setCopied('link');
          setTimeout(() => setCopied(null), 2500);
        }
      });

      const lkRoom = new Room({
        publishDefaults: {
          videoCodec: 'h264',
        },
      });
      liveKitRoomRef.current = lkRoom;

      lkRoom.on(RoomEvent.ParticipantConnected, (participant) => {
        if (!participant.isLocal) {
          setSpectatorCount(lkRoom.remoteParticipants.size);
        }
      });
      lkRoom.on(RoomEvent.ParticipantDisconnected, (participant) => {
        if (!participant.isLocal) {
          setSpectatorCount(lkRoom.remoteParticipants.size);
        }
      });
      lkRoom.on(RoomEvent.Disconnected, () => {
        if (liveKitRoomRef.current === lkRoom) {
          setRoomCode('');
          setShareUrl('');
          setSpectatorCount(0);
          liveKitRoomRef.current = null;
        }
      });

      await lkRoom.connect(resolvedLivekitUrl, token);
    } catch (err) {
      console.error('Failed to create room:', err);
      const message = err instanceof Error ? err.message : 'Failed to create room';
      pushToast({ title: 'Room creation failed', description: message, variant: 'error' });
    }
  };

  // Wayland uses xdg-desktop-portal; X11 uses the in-app source picker.
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

  const handleStartShare = async () => {
    primeAudioContext();
    try {
      const videoTrack = await captureVideoTrack();

      let targetAudioId: number | null = selectedAudioAppId;

      if (targetAudioId === null && !audioAppExplicitlySet) {
        await loadAudioApps();

        const app = await attemptAutoResolve(
          isWayland ? {} : { sourceId: selectedSourceId, nameHint: videoTrack.label },
        );
        targetAudioId = app?.id ?? null;
      }

      if (targetAudioId === null) {
        setAutoDetectFailed(true);
        let ctx: CaptureContext | null = null;
        if (isWayland && window.electronAPI?.getCaptureContext) {
          ctx = await window.electronAPI.getCaptureContext();
          setCaptureContext(ctx);
        }

        const isMonitor = ctx?.sourceType === 'monitor' || (!isWayland && selectedSourceId?.startsWith('screen:'));

        if (isMonitor || (isWayland && ctx?.de === 'kde')) {
          setAutoDetectFailed(false);
          targetAudioId = -1;
          console.log('[Presenter] No specific app resolved — using system audio (desktop audio fallback)');
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
          audioAppIdRef.current = targetAudioId;
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

      // Publish to LiveKit
      const room = liveKitRoomRef.current;
      if (!room) {
        throw new Error('Not connected to a room');
      }

      // Unpublish any previous tracks before re-publishing (e.g. after restart).
      let hadExisting = false;
      for (const pub of room.localParticipant.trackPublications.values()) {
        const t = pub.track;
        if (t) {
          await room.localParticipant.unpublishTrack(t);
          hadExisting = true;
        }
      }

      if (hadExisting) {
        // Give the SDP renegotiation a moment to settle before publishing.
        await new Promise((r) => setTimeout(r, 100));
      }

      // Reset stats accumulator for fresh telemetry.
      statsPrevRef.current = { vBytes: 0, vFrames: 0, vTs: 0, vInit: false, aBytes: 0, aTs: 0, aInit: false };

      // `screenShareEncoding: undefined` overrides LiveKit's 2.5 Mbps default
      // preset so it negotiates the track without a target bitrate. With one,
      // it munges `x-google-start-bitrate` into the sending m-section's fmtp,
      // which then disagrees with the recvonly placeholder sections LiveKit
      // pre-populates for the same VP8 payload type — libwebrtc rejects that as
      // a bundled payload type collision (RFC 8843). The encoder parameters we
      // actually want are applied on the sender right after publishing.
      await room.localParticipant.publishTrack(videoTrack, {
        source: Track.Source.ScreenShare,
        screenShareEncoding: undefined,
        simulcast: false,
      });

      if (audioTrack) {
        await room.localParticipant.publishTrack(audioTrack, {
          source: Track.Source.ScreenShareAudio,
        });
      }

      setIsSharing(true);
      isSharingRef.current = true;

      setTelemetry({ ...idleTelemetry(), live: true });
      startTelemetryPolling();
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

    // Unpublish tracks from LiveKit but keep the room connected.
    const room = liveKitRoomRef.current;
    if (room) {
      for (const pub of room.localParticipant.trackPublications.values()) {
        const t = pub.track;
        if (t) {
          await room.localParticipant.unpublishTrack(t);
        }
      }
    }

    if (telemetryPollRef.current) {
      clearInterval(telemetryPollRef.current);
      telemetryPollRef.current = null;
    }
    broadcastStartRef.current = null;
    bitrateHistoryRef.current = [];
    setTelemetry(idleTelemetry());
    isSharingRef.current = false;
    audioAppIdRef.current = null;
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
    const url = shareUrl;
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
            <h1 className="text-lg font-bold text-gray-100 shrink-0 tracking-tight">Slopcast</h1>
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
                  <span className="hidden sm:inline-flex items-center rounded-full px-2.5 py-1 text-xs font-medium text-muted-foreground bg-gray-900/80 border border-accent shrink-0 tabular-nums">
                    {spectatorCount} spectator{spectatorCount === 1 ? '' : 's'}
                  </span>
                )}
                <button
                  type="button"
                  onClick={handleCopyCode}
                  className="flex items-center gap-2 bg-gray-900/80 border border-gray-800 px-3 py-1.5 rounded-lg text-xs focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background transition-colors"
                >
                  <span className="text-gray-400 font-mono">{roomCode}</span>
                  <span className="text-gray-200 bg-accent/50 px-2 py-0.5 rounded">
                    {copied === 'code' ? 'Copied' : 'Copy'}
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
              aria-label="Screen share preview"
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
              streamed. Select <strong className="text-gray-300">Desktop Audio</strong> to capture all system sound.
            </p>

            <div className="space-y-1.5 max-h-56 overflow-y-auto pr-1">
              {(() => {
                const displayName = (app: AudioApp): string => app.name;
                const groupSubLabel = (group: AudioAppGroup, isDesktopAudio: boolean): string | null => {
                  if (isDesktopAudio) return 'All system audio';
                  const { members } = group;
                  let label: string | undefined;
                  for (const m of members) {
                    if (m.mediaTitle) {
                      label = m.mediaTitle;
                      break;
                    }
                  }
                  if (!label) {
                    for (const m of members) {
                      if (m.windowTitle) {
                        label = m.windowTitle;
                        break;
                      }
                    }
                  }
                  if (label) {
                    if (members.length > 1) return `${label} \u00B7 ${members.length} streams`;
                    return label;
                  }
                  if (members.length > 1) return `${members.length} audio streams`;
                  return null;
                };

                const renderBtn = (group: AudioAppGroup, isDesktopAudio: boolean) => {
                  const { representative, members } = group;
                  const isSelected = members.some((m) => m.id === selectedAudioAppId);
                  const isAutoDetected = members.some((m) => m.id === autoDetectedApp?.id);
                  const level = members.reduce((max, m) => Math.max(max, audioLevels.get(m.id) ?? 0), 0);
                  const btnClass = `flex items-center justify-between p-2.5 rounded-lg border text-xs transition-all cursor-pointer text-left w-full focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background ${
                    isSelected
                      ? isDesktopAudio
                        ? 'bg-amber-950/40 border-amber-500/40 text-amber-200'
                        : 'bg-emerald-950/40 border-emerald-500/40 text-emerald-200'
                      : 'bg-background/60 border-gray-800/60 text-gray-400 hover:border-gray-700 hover:text-gray-300'
                  }`;
                  return (
                    <button
                      key={representative.id}
                      type="button"
                      onClick={() => {
                        if (isSelected) {
                          setAudioAppExplicitlySet(false);
                          setSelectedAudioAppId(null);
                          setAutoDetectedApp(null);
                        } else {
                          setAudioAppExplicitlySet(true);
                          setSelectedAudioAppId(representative.id);
                          setAutoDetectedApp(null);
                        }
                      }}
                      className={btnClass}
                    >
                      <div className="min-w-0 flex-1">
                        <div className="flex items-center gap-2">
                          <span className="font-semibold truncate min-w-0">{displayName(representative)}</span>
                          {!isDesktopAudio && <AudioLevelMeter level={level} />}
                        </div>
                        {(() => {
                          const label = groupSubLabel(group, isDesktopAudio);
                          if (!label) return null;
                          return (
                            <div className="flex items-center gap-2 mt-0.5">
                              <span className="text-[10px] opacity-60">{label}</span>
                              {isAutoDetected && (
                                <span className="text-[10px] bg-safelight-glow text-safelight/80 px-1.5 py-0.5 rounded-full">
                                  auto
                                </span>
                              )}
                            </div>
                          );
                        })()}
                      </div>
                      {isSelected && (
                        <Check
                          className={`w-4 h-4 shrink-0 ${isDesktopAudio ? 'text-amber-300' : 'text-emerald-300'}`}
                          aria-hidden="true"
                        />
                      )}
                    </button>
                  );
                };

                const desktopAudio: AudioApp = {
                  id: -1,
                  name: 'Desktop Audio (All System Sound)',
                  processId: 0,
                  clientId: null,
                  mediaTitle: null,
                };
                const items: React.ReactNode[] = [
                  renderBtn({ representative: desktopAudio, members: [desktopAudio] }, true),
                ];
                if (audioAppGroups.length > 0) {
                  items.push(<div key="divider" className="border-t border-gray-800 my-1.5" />);
                  for (const group of audioAppGroups) {
                    items.push(renderBtn(group, false));
                  }
                }
                return items.length > 0 ? (
                  items
                ) : (
                  <p className="text-xs text-gray-600 text-center py-6">No active audio applications detected</p>
                );
              })()}
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
                  <p className="text-gray-400 bg-gray-800/40 border border-gray-700/40 rounded-lg p-2.5 leading-relaxed">
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
                      aria-label={source.name}
                      className={`p-2 rounded-lg border cursor-pointer transition-all text-xs text-center space-y-1.5 w-full focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background ${
                        isSelected
                          ? 'bg-gray-800/50 border-gray-600 ring-1 ring-gray-600/30'
                          : 'bg-background/60 border-gray-800/60 hover:border-gray-700'
                      }`}
                    >
                      <img
                        src={source.thumbnail}
                        alt=""
                        className="w-full h-20 object-cover rounded"
                        aria-hidden="true"
                      />
                      <span className="block font-medium truncate text-gray-300">{source.name}</span>
                    </button>
                  );
                })}
              </div>
            )}

            {autoDetectFailed && captureContext?.de === 'kde' && (
              <div className="bg-gray-800/50 border border-gray-700/50 rounded-lg p-3 space-y-1">
                <p className="text-xs font-semibold text-gray-200">KDE Audio Auto-Detection Failed</p>
                <p className="text-[11px] text-gray-500 leading-relaxed">
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
        <div className="bg-gray-900/80 border border-gray-800 rounded-xl">
          <button
            type="button"
            onClick={() => setStreamSettingsOpen((v) => !v)}
            className={`flex w-full items-center justify-between gap-3 px-6 pt-6 text-left focus:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-safelight/70 ${
              streamSettingsOpen ? 'pb-0' : 'pb-6'
            }`}
            aria-expanded={streamSettingsOpen}
          >
            <div>
              <h2 className="text-xs font-semibold uppercase tracking-wider text-gray-400">Stream Settings</h2>
              <p className="text-xs text-gray-500 leading-relaxed mt-1">
                Changes apply in real time — no restart needed.
              </p>
            </div>
            <ChevronDown
              className={`size-4 shrink-0 text-gray-500 transition-transform duration-200 ${
                streamSettingsOpen ? 'rotate-0' : '-rotate-90'
              }`}
            />
          </button>
          <div
            className={`overflow-hidden transition-all duration-200 ease-out ${
              streamSettingsOpen ? 'max-h-[600px] opacity-100' : 'max-h-0 opacity-0'
            }`}
          >
            <div className="space-y-5 p-6">
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
                    className="w-full rounded-lg bg-background/90 border border-gray-800 text-sm text-gray-200 py-2 px-3 focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background cursor-pointer"
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
                    className="w-full rounded-lg bg-background/90 border border-gray-800 text-sm text-gray-200 py-2 px-3 focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background cursor-pointer"
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
                    className="w-full rounded-lg bg-background/90 border border-gray-800 text-sm text-gray-200 py-2 px-3 focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background cursor-pointer"
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

              <div className="space-y-1.5">
                <label
                  htmlFor="api-endpoint"
                  className="block text-[10px] font-semibold uppercase tracking-[0.05em] text-gray-500"
                >
                  API Endpoint
                </label>
                <input
                  id="api-endpoint"
                  type="text"
                  value={apiEndpoint}
                  onChange={(e) => setApiEndpoint(e.target.value)}
                  placeholder="http://localhost:3001"
                  className="w-full rounded-lg bg-background/90 border border-gray-800 text-sm text-gray-200 py-2 px-3 focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background font-mono"
                />
              </div>
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
