import type { AudioApp, ResolutionPreset, StreamSettings, VideoCodec, VideoSourceType } from '@slopcast/shared-types';
import {
  codecLabel,
  DEFAULT_STREAM_SETTINGS,
  RESOLUTION_DIMENSIONS,
  VIDEO_CODEC_PRIORITY,
} from '@slopcast/shared-types';
import { Track } from 'livekit-client';
import { Pause, Play, ScreenShare, X } from 'lucide-react';
import type React from 'react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Toaster } from '@/components/ui/sonner';
import { AudioAppPicker } from './components/audio/AudioAppPicker';
import { WelcomeBanner } from './components/onboarding/WelcomeBanner';
import { type CodecInfo, StreamSettingsPanel } from './components/settings/StreamSettingsPanel';
import {
  fmtDuration,
  idleTelemetry,
  type StreamTelemetry,
  StreamTelemetryBar,
} from './components/telemetry/StreamTelemetryBar';
import { Badge } from './components/ui/badge';
import { useLiveKitRoom } from './hooks/useLiveKitRoom';
import { notify, primeAudioContext } from './lib/toast';
import { groupAudioApps } from './utils/audio-grouping';
import './types/electron-api.d.ts';
import './index.css';

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

// The audio app list re-enumerates PipeWire on this cadence; settings writes
// to disk are debounced so rapid changes coalesce into one save + one toast.
const AUDIO_APPS_POLL_MS = 3000;
const SETTINGS_SAVE_DEBOUNCE_MS = 800;

const streamSettingsEqual = (a: StreamSettings, b: StreamSettings): boolean =>
  a.fps === b.fps &&
  a.bitrateLimit === b.bitrateLimit &&
  a.videoCodec === b.videoCodec &&
  a.resolution === b.resolution &&
  a.apiEndpoint === b.apiEndpoint &&
  a.sourceType === b.sourceType &&
  a.videoFilePath === b.videoFilePath;

const KNOWN_VIDEO_CODECS: Record<string, { codec: VideoCodec; label: string }> = {
  'VIDEO/AV1': { codec: 'av1', label: 'AV1' },
  'VIDEO/H264': { codec: 'h264', label: 'H.264' },
  'VIDEO/VP9': { codec: 'vp9', label: 'VP9' },
  'VIDEO/VP8': { codec: 'vp8', label: 'VP8' },
};

// Multiple profile/level variants per family: isConfigSupported validates the
// codec string's level against the requested resolution, so probing a single
// low-level string (e.g. avc1.42E01E at 1080p) falsely reports "unsupported"
// even when the hardware encoder handles the family at higher levels.
const WEBCODECS_PROBE_CODECS: Record<VideoCodec, string[]> = {
  vp8: ['vp8'],
  h264: ['avc1.640028', 'avc1.4D4028', 'avc1.42E028'],
  vp9: ['vp09.00.40.08', 'vp09.00.41.08'],
  av1: ['av01.0.08M.08', 'av01.0.09M.08'],
};

const sortByEncodingEfficiency = (codecs: CodecInfo[]): CodecInfo[] => {
  const priority = new Map<VideoCodec, number>(VIDEO_CODEC_PRIORITY.map((c, i) => [c, i]));
  return [...codecs].sort((a, b) => (priority.get(a.codec) ?? 99) - (priority.get(b.codec) ?? 99));
};

// Every codec Chromium's WebRTC stack can send on this device. Hardware
// acceleration is probed separately (async) via WebCodecs.
function detectSupportedCodecs(): CodecInfo[] {
  const caps = RTCRtpSender.getCapabilities('video');
  if (!caps) {
    return [{ codec: 'h264', label: 'H.264', hardware: false, recommended: false }];
  }

  const mimeTypes = new Set(caps.codecs.map((c) => c.mimeType.toUpperCase()));
  const available: CodecInfo[] = [];
  for (const [mime, info] of Object.entries(KNOWN_VIDEO_CODECS)) {
    if (mimeTypes.has(mime) && !available.some((c) => c.codec === info.codec)) {
      available.push({ ...info, hardware: false, recommended: false });
    }
  }
  return sortByEncodingEfficiency(available);
}

const supportsHardwareEncoding = async (codec: VideoCodec): Promise<boolean> => {
  for (const probe of WEBCODECS_PROBE_CODECS[codec]) {
    try {
      const { supported } = await VideoEncoder.isConfigSupported({
        codec: probe,
        width: 1920,
        height: 1080,
        bitrate: 6_000_000,
        framerate: 30,
        hardwareAcceleration: 'prefer-hardware',
      });
      if (supported) return true;
    } catch (err) {
      console.warn(`[Codecs] hardware probe failed for ${codec} (${probe}):`, err);
    }
  }
  return false;
};

// Tags each codec with hardware-acceleration availability, then hoists the
// most efficient hardware encoder to the top as the recommended choice.
const probeCodecHardware = async (codecs: CodecInfo[]): Promise<CodecInfo[]> => {
  const probed = await Promise.all(
    codecs.map(async (info) => ({ ...info, hardware: await supportsHardwareEncoding(info.codec) })),
  );
  const recommended = probed.find((c) => c.hardware);
  if (!recommended) return probed;
  return [{ ...recommended, recommended: true }, ...probed.filter((c) => c.codec !== recommended.codec)];
};

const codecOptionSuffix = (info: CodecInfo): string => {
  if (info.recommended) return 'Hardware - Recommended';
  return info.hardware ? 'Hardware' : 'Software';
};

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

export const PresenterApp: React.FC = () => {
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
  const [showStopConfirm, setShowStopConfirm] = useState(false);
  const [copied, setCopied] = useState<'link' | 'code' | null>(null);
  const [previewStream, setPreviewStream] = useState<MediaStream | null>(null);
  const [captureContext, setCaptureContext] = useState<CaptureContext | null>(null);
  const [autoDetectFailed, setAutoDetectFailed] = useState(false);
  const [telemetry, setTelemetry] = useState<StreamTelemetry>(idleTelemetry());

  // ── Video file streaming ─────────────────────────────────────────────
  const [selectedSourceType, setSelectedSourceType] = useState<VideoSourceType>('screen');
  const [selectedVideoFilePath, setSelectedVideoFilePath] = useState<string | null>(null);
  const [selectedVideoFileName, setSelectedVideoFileName] = useState<string | null>(null);
  const [videoFileLoop, setVideoFileLoop] = useState(false);
  const [videoDuration, setVideoDuration] = useState(0);
  const [videoCurrentTime, setVideoCurrentTime] = useState(0);
  const [videoIsPlaying, setVideoIsPlaying] = useState(false);
  const [videoFileError, setVideoFileError] = useState<string | null>(null);

  const [timelineHoverTime, setTimelineHoverTime] = useState<number | null>(null);
  const [timelineHoverRatio, setTimelineHoverRatio] = useState<number>(0);

  const videoCurrentTimeRef = useRef(0);
  const videoIsPlayingRef = useRef(false);
  const videoDurationRef = useRef(0);
  const videoFileLoopRef = useRef(false);
  const lastTickWallMsRef = useRef<number | null>(null);

  useEffect(() => {
    videoIsPlayingRef.current = videoIsPlaying;
  }, [videoIsPlaying]);

  useEffect(() => {
    videoDurationRef.current = videoDuration;
  }, [videoDuration]);

  useEffect(() => {
    videoFileLoopRef.current = videoFileLoop;
  }, [videoFileLoop]);

  // ── Stream Settings (user-configurable encoder parameters) ───────────
  const [streamSettingsOpen, setStreamSettingsOpen] = useState(false);
  const [streamFps, setStreamFps] = useState(DEFAULT_STREAM_SETTINGS.fps);
  const [bitrateLimit, setBitrateLimit] = useState(DEFAULT_STREAM_SETTINGS.bitrateLimit);
  const [availableCodecs, setAvailableCodecs] = useState(() => detectSupportedCodecs());
  const [videoCodec, setVideoCodec] = useState<VideoCodec>(() => {
    const top = availableCodecs[0]?.codec ?? 'h264';
    const saved = DEFAULT_STREAM_SETTINGS.videoCodec;
    return availableCodecs.some((c) => c.codec === saved) ? saved : top;
  });
  const [resolution, setResolution] = useState<ResolutionPreset>(DEFAULT_STREAM_SETTINGS.resolution);

  const {
    roomCode,
    shareUrl,
    spectatorCount,
    isCreatingRoom,
    liveKitRoomRef,
    createRoom: createLiveKitRoom,
  } = useLiveKitRoom({
    apiEndpoint,
    livekitUrl,
    videoCodec,
  });

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
  const streamFpsRef = useRef(DEFAULT_STREAM_SETTINGS.fps);
  const bitrateLimitRef = useRef(DEFAULT_STREAM_SETTINGS.bitrateLimit);
  const resolutionRef = useRef(DEFAULT_STREAM_SETTINGS.resolution);
  const audioAppIdRef = useRef<number | null>(null);
  // Gates the persistence effect until the saved file has been loaded, and
  // remembers the last written values so hydration never triggers a re-save.
  const settingsHydratedRef = useRef(false);
  const lastSavedSettingsRef = useRef<StreamSettings | null>(null);

  const videoFileTimeUpdateRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const videoFileGenRef = useRef<{
    video: MediaStreamTrackGenerator;
    audio: MediaStreamTrackGenerator | null;
    videoWriter: WritableStreamDefaultWriter<VideoFrame>;
    audioWriter: WritableStreamDefaultWriter<AudioData> | null;
    cleanup: () => void;
  } | null>(null);

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
    resolutionRef.current = resolution;
  }, [resolution]);

  useEffect(() => {
    audioAppIdRef.current = selectedAudioAppId;
  }, [selectedAudioAppId]);

  // Persist stream settings to disk. Writes are debounced so a burst of
  // changes (e.g. typing in the API endpoint field) coalesces into a single
  // save — and therefore a single "saved" notification.
  useEffect(() => {
    if (!settingsHydratedRef.current) return;
    const current: StreamSettings = {
      fps: streamFps,
      bitrateLimit,
      videoCodec,
      resolution,
      apiEndpoint,
      sourceType: selectedSourceType,
      videoFilePath: selectedVideoFilePath ?? undefined,
    };
    const last = lastSavedSettingsRef.current;
    if (last && streamSettingsEqual(last, current)) return;

    const timer = setTimeout(() => {
      void window.electronAPI
        ?.saveStreamSettings(current)
        .then((ok) => {
          if (!ok) {
            notify('error', 'Settings save failed', 'The settings file could not be written to disk.');
            return;
          }
          lastSavedSettingsRef.current = current;
          notify('success', 'Stream settings saved');
        })
        .catch((err: unknown) => {
          console.error('[Presenter] saveStreamSettings IPC failed:', err);
        });
    }, SETTINGS_SAVE_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [streamFps, bitrateLimit, videoCodec, resolution, apiEndpoint, selectedSourceType, selectedVideoFilePath]);

  const captureAudioTrack = useCallback(async (targetId: number): Promise<MediaStreamTrack | null> => {
    const started = await window.electronAPI?.startAudioCapture(targetId);
    if (!started) {
      throw new Error('Native audio capture failed to start');
    }

    // Best-effort label unlock: without mic permission the virtual source's
    // label stays hidden and findCaptureAudioDevice cannot match it.
    const unlock = await navigator.mediaDevices.getUserMedia({ audio: true }).catch((err) => {
      console.warn('[Presenter] mic permission denied; device labels may stay hidden:', err);
      return null;
    });
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

  const replaceAudioTrack = useCallback(
    async (targetId: number): Promise<void> => {
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
    },
    [liveKitRoomRef],
  );

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
        notify('error', 'Audio switch failed', 'Could not switch to the selected audio source.');
      }
    };
    void switchAudio();
  }, [selectedAudioAppId, isSharing, replaceAudioTrack]);

  // Push live encoder parameter updates via the published track's sender.
  useEffect(() => {
    if (!isSharing) return;
    const fps = streamFps;
    const br = bitrateLimit;
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
          enc.scaleResolutionDownBy = 1.0;
          enc.priority = 'high';
          enc.networkPriority = 'high';
          enc.active = true;
        }
        await sender.setParameters(params);
        console.log(`[Presenter] Live encoder update: fps=${fps} bitrate=${(br / 1_000_000).toFixed(0)}Mbps`);

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
  }, [streamFps, bitrateLimit, isSharing, replaceAudioTrack, liveKitRoomRef]);

  // Bind the live capture stream to the local preview <video>.
  useEffect(() => {
    const el = previewVideoRef.current;
    if (!el) return;
    el.srcObject = previewStream;
    if (previewStream) {
      el.muted = selectedSourceType !== 'video-file';
      el.play().catch(() => console.warn('Video autoplay blocked until user gesture'));
    }
  }, [previewStream, selectedSourceType]);

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

        const codecs = await probeCodecHardware(detectSupportedCodecs());
        setAvailableCodecs(codecs);
        console.log(
          '[Presenter] App start — codecs:',
          codecs.map((c) => `${c.label} (${codecOptionSuffix(c)})`).join(', '),
        );

        // Persisted settings take precedence over config-file defaults.
        const saved = await window.electronAPI.getStreamSettings();
        lastSavedSettingsRef.current = saved;
        setStreamFps(saved.fps);
        setBitrateLimit(saved.bitrateLimit);
        const bestCodec = codecs[0]?.codec ?? 'h264';
        const savedOk = codecs.some((c) => c.codec === saved.videoCodec);
        setVideoCodec(savedOk ? saved.videoCodec : bestCodec);
        setResolution(saved.resolution);
        setApiEndpoint(saved.apiEndpoint);
        if (saved.sourceType) setSelectedSourceType(saved.sourceType);
        if (saved.videoFilePath) {
          setSelectedVideoFilePath(saved.videoFilePath);
          const name = saved.videoFilePath.split(/[\\/]/).pop() ?? null;
          setSelectedVideoFileName(name);
        }
        settingsHydratedRef.current = true;
      }
      loadAudioApps();
    })();

    return () => {
      liveKitRoomRef.current?.disconnect();
      liveKitRoomRef.current = null;
      if (telemetryPollRef.current) {
        clearInterval(telemetryPollRef.current);
        telemetryPollRef.current = null;
      }
    };
  }, [loadAudioApps, loadDesktopSources, liveKitRoomRef]);

  // Keep the audio source list fresh. listAudioApplications() is a read-only
  // PipeWire enumeration on a throwaway connection — it never touches the
  // capture or metering sessions, and the selection lives in separate state
  // keyed by stable node ids, so an active stream is never interrupted.
  useEffect(() => {
    const interval = setInterval(() => {
      void loadAudioApps();
    }, AUDIO_APPS_POLL_MS);
    return () => clearInterval(interval);
  }, [loadAudioApps]);

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
      void api.stopAudioMetering().catch((err) => console.warn('[Presenter] stopAudioMetering failed:', err));
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
  };

  const handleCreateRoom = async () => {
    setAudioAppExplicitlySet(false);
    setAutoDetectedApp(null);
    setSelectedAudioAppId(null);
    setAutoDetectFailed(false);

    await createLiveKitRoom();
  };

  // video-file playback, which delivers its audio directly from
  // MediaStream.captureStream() instead of PipeWire.
  const preCheckVideoFile = useCallback(async (filePath: string) => {
    if (!window.electronAPI?.probeVideoFile) {
      setVideoFileError('FFmpeg is not available (video file playback not supported)');
      return;
    }
    try {
      const info = await window.electronAPI.probeVideoFile(filePath);
      if (!info) {
        setVideoFileError(`Cannot open file. The format may be unsupported or the file is corrupt.`);
        return;
      }
      setVideoFileError(null);
      setVideoDuration(info.durationMs / 1000);
    } catch {
      setVideoFileError(`Cannot open file. The format may be unsupported or the file is corrupt.`);
    }
  }, []);

  const captureVideoTrackFromFile = async (
    filePath: string,
  ): Promise<{ videoTrack: MediaStreamTrack; audioTrack: MediaStreamTrack | null }> => {
    if (!window.electronAPI) {
      throw new Error('Electron API not available');
    }

    const videoGenerator = new MediaStreamTrackGenerator({ kind: 'video' });
    const videoWriter = videoGenerator.writable.getWriter();
    let audioWriter: WritableStreamDefaultWriter<AudioData> | null = null;
    let audioGenerator: MediaStreamTrackGenerator | null = null;
    let width: number | null = null;
    let height: number | null = null;

    // Try to pre-create the audio generator — we create it eagerly if the
    // FFmpeg thread produces audio before the first video frame (which is
    // how many files are structured).
    let earlyAudioData: Uint8Array[] = [];
    let audioGeneratorReady = false;

    const startWallUs = performance.now() * 1000;
    let totalAudioSamples = 0;

    const createAudioGenerator = () => {
      if (audioGeneratorReady) return;
      audioGenerator = new MediaStreamTrackGenerator({ kind: 'audio' });
      audioWriter = audioGenerator.writable.getWriter();
      audioGeneratorReady = true;

      // Flush any early audio data
      let audioFrameIdx = 0;
      for (const buf of earlyAudioData) {
        if (buf.length >= 4) {
          const samples = Math.floor(buf.length / 4);
          const tsUs = Math.round((audioFrameIdx / 48000) * 1_000_000);
          audioFrameIdx += samples;
          const audioData = new AudioData({
            format: 's16',
            sampleRate: 48000,
            numberOfChannels: 2,
            numberOfFrames: samples,
            timestamp: tsUs,
            data: new Uint8Array(buf.buffer, buf.byteOffset, buf.length) as BufferSource,
          });
          audioWriter?.write(audioData).catch(console.error);
          audioData.close();
        }
      }
      totalAudioSamples += audioFrameIdx;
      earlyAudioData = [];
    };

    const unsubFrame = window.electronAPI.onVideoFileFrame((data) => {
      if (!data) {
        videoWriter.close().catch(console.error);
        audioWriter?.close().catch(console.error);
        return;
      }

      if (width === null || height === null) return;

      const elapsedUs = Math.round(performance.now() * 1000 - startWallUs);
      const frame = new VideoFrame(new Uint8Array(data), {
        format: 'RGBA',
        codedWidth: width,
        codedHeight: height,
        timestamp: Math.max(0, elapsedUs),
      });
      videoWriter.write(frame).catch(console.error);
      frame.close();
    });

    const unsubAudio = window.electronAPI.onVideoFileAudio((data) => {
      if (!data) {
        audioWriter?.close().catch(console.error);
        return;
      }

      if (!audioGeneratorReady) {
        if (data.length > 0) {
          earlyAudioData.push(data);
        }
        return;
      }

      if (!audioWriter || data.length < 4) return;

      const buf = new Uint8Array(data);
      const samples = Math.floor(buf.length / 4);
      if (samples === 0) return;

      const tsUs = Math.round((totalAudioSamples / 48000) * 1_000_000);
      totalAudioSamples += samples;

      const audioData = new AudioData({
        format: 's16',
        sampleRate: 48000,
        numberOfChannels: 2,
        numberOfFrames: samples,
        timestamp: tsUs,
        data: new Uint8Array(buf.buffer, buf.byteOffset, buf.length) as BufferSource,
      });
      audioWriter.write(audioData).catch(console.error);
      audioData.close();
    });

    // Probe to get dimensions
    const info = await window.electronAPI.probeVideoFile(filePath);
    if (!info) throw new Error('Failed to probe video file');
    width = info.width;
    height = info.height;

    // Create audio generator if the file has audio
    if (info.hasAudio) {
      createAudioGenerator();
    }

    const started = await window.electronAPI.startVideoFile(filePath);
    if (!started) throw new Error('Failed to start FFmpeg video file playback');

    const durSec = info.durationMs / 1000;
    setVideoDuration(durSec);
    videoDurationRef.current = durSec;
    setVideoIsPlaying(true);
    videoIsPlayingRef.current = true;
    videoCurrentTimeRef.current = 0;
    setVideoCurrentTime(0);
    lastTickWallMsRef.current = performance.now();

    if (videoFileTimeUpdateRef.current) clearInterval(videoFileTimeUpdateRef.current);
    videoFileTimeUpdateRef.current = setInterval(() => {
      if (!lastTickWallMsRef.current) {
        lastTickWallMsRef.current = performance.now();
        return;
      }
      const now = performance.now();
      const delta = (now - lastTickWallMsRef.current) / 1000;
      lastTickWallMsRef.current = now;

      if (videoIsPlayingRef.current) {
        let nextTime = videoCurrentTimeRef.current + delta;
        const dur = videoDurationRef.current;
        if (dur > 0 && nextTime >= dur) {
          if (videoFileLoopRef.current) {
            nextTime = 0;
            if (window.electronAPI) {
              window.electronAPI.seekVideoFile(0);
            }
          } else {
            nextTime = dur;
            setVideoIsPlaying(false);
            videoIsPlayingRef.current = false;
            if (window.electronAPI) {
              window.electronAPI.pauseVideoFile(true);
            }
          }
        }
        videoCurrentTimeRef.current = nextTime;
        setVideoCurrentTime(nextTime);
      }
    }, 100);

    // Add a cleanup record for handleStopShare to use
    const cleanup = () => {
      videoWriter.close().catch(console.error);
      audioWriter?.close().catch(console.error);
      unsubFrame();
      unsubAudio();
    };

    videoFileGenRef.current = {
      video: videoGenerator,
      audio: audioGenerator,
      videoWriter,
      audioWriter,
      cleanup,
    };

    return {
      videoTrack: videoGenerator,
      audioTrack: audioGenerator,
    };
  };

  // Wayland uses xdg-desktop-portal; X11 uses the in-app source picker.
  // Video-file case is handled directly in handleStartShare — it returns
  // both video+audio tracks and creates side effects (AudioContext, interval).
  const captureVideoTrack = async (): Promise<MediaStreamTrack> => {
    const dims = RESOLUTION_DIMENSIONS[resolutionRef.current];
    if (isWayland) {
      // The main-process displayMediaRequestHandler answers this request;
      // xdg-desktop-portal shows the desktop environment's own window picker.
      const fps = streamFpsRef.current;
      const stream = await navigator.mediaDevices.getDisplayMedia({
        video: {
          frameRate: { ideal: fps, max: fps },
          width: { ideal: dims.width, max: dims.width },
          height: { ideal: dims.height, max: dims.height },
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
          maxWidth: dims.width,
          maxHeight: dims.height,
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
      // ── Video file streaming — captureStream() provides both tracks ──
      if (selectedSourceType === 'video-file') {
        if (!selectedVideoFilePath) {
          notify('error', 'No file selected', 'Please select a video file first.');
          return;
        }

        const { videoTrack, audioTrack } = await captureVideoTrackFromFile(selectedVideoFilePath);

        const tracks = audioTrack ? [videoTrack, audioTrack] : [videoTrack];
        const stream = new MediaStream(tracks);
        localStreamRef.current = stream;
        setPreviewStream(stream);

        videoTrack.onended = () => handleStopShare();

        const room = liveKitRoomRef.current;
        if (!room) {
          throw new Error('Not connected to a room');
        }

        for (const pub of room.localParticipant.trackPublications.values()) {
          const t = pub.track;
          if (t) await room.localParticipant.unpublishTrack(t);
        }

        statsPrevRef.current = { vBytes: 0, vFrames: 0, vTs: 0, vInit: false, aBytes: 0, aTs: 0, aInit: false };

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

        return;
      }

      // ── Screen capture — getDisplayMedia / getUserMedia ──────────────
      const videoTrack = await captureVideoTrack();

      let targetAudioId: number | null = selectedAudioAppId;

      if (targetAudioId === null && !audioAppExplicitlySet) {
        await loadAudioApps();

        const app = await attemptAutoResolve(
          isWayland ? { nameHint: videoTrack.label } : { sourceId: selectedSourceId, nameHint: videoTrack.label },
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
          notify('info', 'No audio detected', 'Sharing video only. Select an audio app and restart to include audio.');
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
          notify('info', 'Audio unavailable', 'Sharing video only — the selected audio source could not be captured.');
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

      if (videoCodec === 'h264') {
        videoTrack.contentHint = 'motion';
        try {
          const publisher = (
            room as {
              engine?: {
                pcManager?: { publisher?: { getLocalDescription(): RTCSessionDescription | null | undefined } };
              };
            }
          ).engine?.pcManager?.publisher;
          if (publisher) {
            const desc = publisher.getLocalDescription();
            if (desc) {
              const h264Lines = desc.sdp
                .split('\n')
                .filter((line) => line.startsWith('a=fmtp:') && line.includes('profile-level-id'));
              for (const line of h264Lines) {
                console.log(`[SDP:send] H264 fmtp: ${line}`);
              }
            }
          }
        } catch {
          console.log('[Presenter] SDP log skipped (no local description yet)');
        }
      }

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
      notify('error', 'Screenshare failed to start', message);
      if (window.electronAPI) {
        await window.electronAPI.stopAudioCapture();
      }
      // Stop any tracks acquired before the failure (getDisplayMedia/getUserMedia)
      // so the OS capture indicator and hardware are released.
      const failedStream = localStreamRef.current;
      if (failedStream) {
        for (const track of failedStream.getTracks()) {
          track.stop();
        }
        localStreamRef.current = null;
      }
      setPreviewStream(null);
      // Clean up video-file resources if any were created before the failure
      if (videoFileGenRef.current) {
        videoFileGenRef.current.cleanup();
        videoFileGenRef.current = null;
      }
      if (videoFileTimeUpdateRef.current) {
        clearInterval(videoFileTimeUpdateRef.current);
        videoFileTimeUpdateRef.current = null;
      }
    }
  };

  const handleStopShare = async () => {
    // ── Clean up video file playback ─────────────────────────────────────
    if (videoFileGenRef.current) {
      videoFileGenRef.current.cleanup();
      videoFileGenRef.current = null;
    }
    if (videoFileTimeUpdateRef.current) {
      clearInterval(videoFileTimeUpdateRef.current);
      videoFileTimeUpdateRef.current = null;
    }
    if (window.electronAPI) {
      await window.electronAPI.stopVideoFile();
    }
    videoIsPlayingRef.current = false;
    videoCurrentTimeRef.current = 0;
    videoDurationRef.current = 0;
    setVideoIsPlaying(false);
    setVideoCurrentTime(0);
    setVideoDuration(0);

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
    setVideoFileError(null);
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
      notify('error', 'Copy failed', 'Room link could not be copied.');
    }
  };

  const handleCopyCode = async () => {
    if (!roomCode) return;
    const ok = await copyText(roomCode);
    if (ok) {
      flashCopied('code');
    } else {
      notify('error', 'Copy failed', 'Room code could not be copied.');
    }
  };

  const canStartShare =
    !!roomCode &&
    !isSharing &&
    (isWayland || !!selectedSourceId || (selectedSourceType === 'video-file' && !!selectedVideoFilePath));
  const startDisabledReason = (): string | null => {
    if (isSharing || canStartShare) return null;
    if (!roomCode) return 'Create a live room to start sharing.';
    if (selectedSourceType === 'video-file' && !selectedVideoFilePath)
      return 'Select a video file above to start streaming.';
    return 'Select a window above to start sharing.';
  };
  const disabledReason = startDisabledReason();

  return (
    <div className="min-h-screen flex flex-col">
      {/* ===== Sticky Header ===== */}
      <header className="sticky top-0 z-10 border-b border-border bg-background/80 backdrop-blur-md">
        <div className="max-w-5xl mx-auto px-6 h-14 flex items-center justify-between gap-4">
          <div className="flex items-center gap-3 min-w-0">
            <span className="p-2 bg-safelight/10 rounded-xl text-safelight shrink-0">
              <ScreenShare className="w-5 h-5" aria-hidden="true" />
            </span>
            <h1 className="text-xl font-bold text-foreground shrink-0 leading-tight tracking-tight">Slopcast</h1>
          </div>

          <div className="shrink-0">
            {!roomCode ? (
              <Button onClick={handleCreateRoom} disabled={isCreatingRoom}>
                {isCreatingRoom ? 'Creating Room...' : 'Create Live Room'}
              </Button>
            ) : (
              <div className="flex items-center gap-2">
                {spectatorCount > 0 && (
                  <Badge variant="info" className="hidden sm:inline-flex tabular-nums">
                    {spectatorCount} spectator{spectatorCount === 1 ? '' : 's'}
                  </Badge>
                )}
                <Button variant="secondary" size="sm" onClick={handleCopyCode} className="gap-2">
                  <span className="text-muted-foreground font-mono">{roomCode}</span>
                  <span className="text-foreground bg-accent/50 px-2 py-1 rounded-md">
                    {copied === 'code' ? 'Copied' : 'Copy'}
                  </span>
                </Button>
                <Button size="sm" onClick={handleCopyLink}>
                  {copied === 'link' ? 'Link Copied!' : 'Copy Link'}
                </Button>
              </div>
            )}
          </div>
        </div>
      </header>

      {/* ===== Main Content ===== */}
      {/* First-launch onboarding — dismissible, non-blocking */}
      <div className="max-w-5xl mx-auto w-full px-6 pt-6">
        <WelcomeBanner />
      </div>

      <main className="flex-1 max-w-5xl mx-auto w-full px-6 py-8 space-y-8">
        {/* Screenshare Preview */}
        <Card className="overflow-hidden shadow-2xl">
          <CardContent className="p-0">
            <div className="relative bg-black aspect-video flex items-center justify-center">
              <video
                ref={previewVideoRef}
                autoPlay
                playsInline
                muted={selectedSourceType !== 'video-file'}
                aria-label="Screen share preview"
                className={`w-full h-full object-contain ${isSharing ? 'block' : 'hidden'}`}
              />
              {isSharing && selectedSourceType === 'video-file' && !videoIsPlaying && (
                <div className="absolute inset-0 bg-black/50 backdrop-blur-[2px] flex flex-col items-center justify-center gap-3 pointer-events-none z-10">
                  <div className="p-4 rounded-full bg-background/80 text-foreground border border-border/60 shadow-xl flex items-center justify-center">
                    <Pause className="w-10 h-10 text-safelight fill-safelight/20" aria-hidden="true" />
                  </div>
                  <span className="text-xs font-bold tracking-widest uppercase text-foreground bg-background/90 px-3.5 py-1.5 rounded-full border border-border/60 shadow-md">
                    PAUSED
                  </span>
                </div>
              )}
              {!isSharing && (
                <div className="absolute inset-0 flex flex-col items-center justify-center gap-3 text-center px-6">
                  {!roomCode ? (
                    <>
                      <span className="p-3 rounded-full bg-safelight/10 mb-1 motion-safe:animate-pulse">
                        <ScreenShare className="size-7 text-safelight/60" aria-hidden="true" />
                      </span>
                      <p className="text-sm text-foreground font-semibold">Ready to stream</p>
                      <p className="text-sm text-muted-foreground max-w-xs leading-relaxed">
                        Create a live room to get a shareable link, then select your source and go live.
                      </p>
                    </>
                  ) : (
                    <>
                      <span className="p-3 rounded-full bg-secondary mb-1">
                        <ScreenShare className="size-7 text-muted-foreground" aria-hidden="true" />
                      </span>
                      <p className="text-sm text-foreground font-semibold">
                        {canStartShare ? 'Ready to go live' : 'Select a source to begin'}
                      </p>
                      <p className="text-sm text-muted-foreground max-w-xs leading-relaxed">
                        {(() => {
                          if (!canStartShare)
                            return 'Choose a window, screen, or video file in the Screenshare Source panel.';
                          if (selectedSourceType === 'video-file')
                            return 'Click Start Streaming below to begin broadcasting.';
                          return 'Click Start Screenshare below to begin broadcasting.';
                        })()}
                      </p>
                      <button
                        type="button"
                        onClick={handleCopyLink}
                        className="mt-1 inline-flex items-center gap-1.5 text-xs font-medium text-safelight hover:text-safelight-hover transition-colors focus:outline-none focus-visible:underline"
                      >
                        {copied === 'link' ? 'Link copied' : `Copy link — ${roomCode}`}
                      </button>
                    </>
                  )}
                </div>
              )}
              {isSharing && <StreamTelemetryBar telemetry={telemetry} />}
            </div>
          </CardContent>
        </Card>

        {/* Controls Grid */}
        <div className="grid grid-cols-1 md:grid-cols-2 gap-8">
          {/* Window Audio Capture */}
          <AudioAppPicker
            audioApps={audioApps}
            audioAppGroups={audioAppGroups}
            selectedAudioAppId={selectedAudioAppId}
            autoDetectedApp={autoDetectedApp}
            audioLevels={audioLevels}
            onSelectApp={(appId, explicit) => {
              setAudioAppExplicitlySet(explicit ?? true);
              setSelectedAudioAppId(appId);
              if (!explicit) setAutoDetectedApp(null);
            }}
            onRefresh={loadAudioApps}
          />

          {/* Screenshare Source */}
          <Card>
            <CardHeader>
              <CardTitle className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
                Screenshare Source
              </CardTitle>
            </CardHeader>
            <CardContent className="space-y-4">
              {/* ── Video file source ──────────────────────────────── */}
              <div className="flex gap-2 min-w-0 max-w-full">
                <Button
                  variant={selectedSourceType === 'video-file' ? 'default' : 'outline'}
                  size="sm"
                  className="flex-1 min-w-0 max-w-full overflow-hidden"
                  onClick={async () => {
                    const result = await window.electronAPI?.selectVideoFile();
                    if (result) {
                      setSelectedSourceType('video-file');
                      setSelectedVideoFilePath(result.filePath);
                      setSelectedVideoFileName(result.fileName);
                      setVideoFileError(null);
                      setSelectedSourceId('');
                      preCheckVideoFile(result.filePath);
                    }
                  }}
                  onContextMenu={(e) => {
                    if (selectedVideoFilePath) {
                      e.preventDefault();
                      setSelectedSourceType('screen');
                      setSelectedVideoFilePath(null);
                      setSelectedVideoFileName(null);
                      setVideoFileError(null);
                    }
                  }}
                >
                  <span className="truncate max-w-full block">
                    {selectedVideoFileName ? `Selected: ${selectedVideoFileName}` : 'Stream Video File...'}
                  </span>
                </Button>
                {selectedVideoFilePath && (
                  <Button
                    variant="ghost"
                    size="sm"
                    className="px-2 shrink-0"
                    onClick={() => {
                      setSelectedSourceType('screen');
                      setSelectedVideoFilePath(null);
                      setSelectedVideoFileName(null);
                      setVideoFileError(null);
                    }}
                    aria-label="Deselect video file"
                  >
                    <X className="h-4 w-4" />
                  </Button>
                )}
              </div>

              {(() => {
                if (isWayland) {
                  if (captureContext?.de === 'kde' && !autoDetectFailed) {
                    return (
                      <div className="space-y-2">
                        <p className="text-sm text-muted-foreground bg-secondary border border-border rounded-lg p-3 leading-relaxed">
                          KDE Plasma detected — window identity is unavailable in PipeWire streams. If auto-detection
                          fails, select an audio app manually.
                        </p>
                      </div>
                    );
                  }
                  return null;
                }
                return (
                  <div className="grid grid-cols-2 gap-2 max-h-56 overflow-y-auto pr-1">
                    {desktopSources.map((source) => {
                      const isSelected = source.id === selectedSourceId;
                      return (
                        <button
                          key={source.id}
                          type="button"
                          onClick={() => {
                            setSelectedSourceId(source.id);
                            setSelectedSourceType('screen');
                            setSelectedVideoFilePath(null);
                            setSelectedVideoFileName(null);
                            void attemptAutoResolve({ sourceId: source.id, nameHint: source.name });
                          }}
                          aria-label={source.name}
                          className={`p-2 rounded-lg border cursor-pointer transition-all text-xs text-center space-y-1.5 w-full focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background ${
                            isSelected
                              ? 'bg-secondary border-input ring-1 ring-input/30'
                              : 'bg-background/60 border-border hover:border-input'
                          }`}
                        >
                          <img
                            src={source.thumbnail}
                            alt=""
                            className="w-full h-20 object-cover rounded-md"
                            aria-hidden="true"
                          />
                          <span className="block font-medium truncate text-foreground">{source.name}</span>
                        </button>
                      );
                    })}
                  </div>
                );
              })()}

              {autoDetectFailed && captureContext?.de === 'kde' && (
                <div className="bg-secondary border border-border rounded-lg p-3 space-y-1">
                  <p className="text-xs font-semibold text-foreground">KDE Audio Auto-Detection Failed</p>
                  <p className="text-sm text-muted-foreground leading-relaxed">
                    Select an audio app from the panel above, then stop and restart the screenshare.
                  </p>
                </div>
              )}

              {isSharing ? (
                <div className="space-y-2">
                  {!showStopConfirm ? (
                    <Button variant="destructive" onClick={() => setShowStopConfirm(true)} className="w-full font-bold">
                      {selectedSourceType === 'video-file' ? 'Stop Streaming' : 'Stop Screenshare'}
                    </Button>
                  ) : (
                    <div className="space-y-2">
                      <p className="text-sm text-muted-foreground text-center">
                        {spectatorCount > 0
                          ? `${spectatorCount} spectator${spectatorCount === 1 ? '' : 's'} watching. Stop streaming?`
                          : 'Stop the stream?'}
                      </p>
                      <div className="flex gap-2">
                        <Button
                          variant="destructive"
                          onClick={() => {
                            setShowStopConfirm(false);
                            handleStopShare();
                          }}
                          className="flex-1 font-bold"
                        >
                          Stop
                        </Button>
                        <Button variant="secondary" onClick={() => setShowStopConfirm(false)} className="flex-1">
                          Cancel
                        </Button>
                      </div>
                    </div>
                  )}
                </div>
              ) : (
                <Button
                  variant="default"
                  onClick={handleStartShare}
                  disabled={!canStartShare}
                  className="w-full font-bold"
                >
                  {selectedSourceType === 'video-file' ? 'Start Streaming' : 'Start Screenshare'}
                </Button>
              )}
              {disabledReason && (
                <p id="start-screenshare-hint" className="text-sm text-muted-foreground leading-relaxed">
                  {disabledReason}
                </p>
              )}
            </CardContent>
          </Card>
        </div>

        {/* Video Controls — shown when sharing a video file */}
        {isSharing && selectedSourceType === 'video-file' && (
          <Card>
            <CardHeader>
              <CardTitle className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
                Video Controls
              </CardTitle>
            </CardHeader>
            <CardContent className="space-y-3">
              {videoFileError && <p className="text-xs text-destructive">{videoFileError}</p>}
              <div className="flex items-center gap-3">
                <Button
                  variant="outline"
                  size="sm"
                  onClick={async () => {
                    if (!window.electronAPI) return;
                    if (videoIsPlaying) {
                      await window.electronAPI.pauseVideoFile(true);
                      setVideoIsPlaying(false);
                      videoIsPlayingRef.current = false;
                    } else {
                      await window.electronAPI.pauseVideoFile(false);
                      lastTickWallMsRef.current = performance.now();
                      setVideoIsPlaying(true);
                      videoIsPlayingRef.current = true;
                    }
                  }}
                  className="gap-1.5 shrink-0"
                >
                  {videoIsPlaying ? (
                    <>
                      <Pause className="w-4 h-4 text-safelight" aria-hidden="true" /> Pause
                    </>
                  ) : (
                    <>
                      <Play className="w-4 h-4 text-safelight" aria-hidden="true" /> Play
                    </>
                  )}
                </Button>
                <div className="relative flex-1 group">
                  {timelineHoverTime !== null && videoDuration > 0 && (
                    <div
                      className="absolute -top-8 -translate-x-1/2 bg-popover text-popover-foreground text-xs font-mono px-2 py-0.5 rounded shadow-md border border-border pointer-events-none tabular-nums z-20 whitespace-nowrap"
                      style={{ left: `${timelineHoverRatio * 100}%` }}
                    >
                      {fmtDuration(timelineHoverTime * 1000)}
                    </div>
                  )}
                  <input
                    type="range"
                    min={0}
                    max={videoDuration || 0}
                    step={0.1}
                    value={videoCurrentTime}
                    onMouseMove={(e) => {
                      const rect = e.currentTarget.getBoundingClientRect();
                      if (rect.width > 0 && videoDuration > 0) {
                        const ratio = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
                        setTimelineHoverRatio(ratio);
                        setTimelineHoverTime(ratio * videoDuration);
                      }
                    }}
                    onMouseLeave={() => setTimelineHoverTime(null)}
                    onChange={(e) => {
                      const newTime = Number(e.target.value);
                      videoCurrentTimeRef.current = newTime;
                      setVideoCurrentTime(newTime);
                      lastTickWallMsRef.current = performance.now();
                      if (window.electronAPI) {
                        window.electronAPI.seekVideoFile(newTime * 1000);
                      }
                    }}
                    className="w-full h-1.5 bg-secondary rounded-lg appearance-none cursor-pointer accent-safelight"
                    aria-label="Seek position"
                  />
                  <div className="flex justify-between text-xs text-muted-foreground mt-1 tabular-nums">
                    <span>{fmtDuration(videoCurrentTime * 1000)}</span>
                    <span>{fmtDuration(videoDuration * 1000)}</span>
                  </div>
                </div>
              </div>
              <label className="flex items-center gap-2 text-xs text-muted-foreground cursor-pointer">
                <input
                  type="checkbox"
                  checked={videoFileLoop}
                  onChange={(e) => {
                    setVideoFileLoop(e.target.checked);
                  }}
                  className="rounded accent-safelight"
                />
                Loop video
              </label>
            </CardContent>
          </Card>
        )}

        {/* Stream Settings */}
        <StreamSettingsPanel
          streamSettingsOpen={streamSettingsOpen}
          setStreamSettingsOpen={setStreamSettingsOpen}
          videoCodec={videoCodec}
          setVideoCodec={setVideoCodec}
          availableCodecs={availableCodecs}
          codecOptionSuffix={codecOptionSuffix}
          resolution={resolution}
          setResolution={setResolution}
          streamFps={streamFps}
          setStreamFps={setStreamFps}
          bitrateLimit={bitrateLimit}
          setBitrateLimit={setBitrateLimit}
        />
      </main>

      <Toaster />
    </div>
  );
};

const rootEl = document.getElementById('root');
if (!rootEl) throw new Error('Missing #root element');
const root = createRoot(rootEl);
root.render(<PresenterApp />);
