import type { AudioApp, ResolutionPreset, StreamSettings, VideoCodec } from '@slopcast/shared-types';
import { DEFAULT_STREAM_SETTINGS, RESOLUTION_DIMENSIONS, VIDEO_CODEC_LABEL_LK } from '@slopcast/shared-types';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { primeAudioContext, useToasts } from '../components/ui/Toast';
import { AUDIO_APPS_POLL_MS, SETTINGS_SAVE_DEBOUNCE_MS, streamSettingsEqual } from '../constants';
import type { StreamTelemetry } from '../telemetry/types';
import { idleTelemetry } from '../telemetry/types';
import type { CaptureContext, DesktopSource } from '../types';
import { groupAudioApps } from '../utils/audio-grouping';
import { copyText } from '../utils/clipboard';

const STATS_POLL_MS = 1000;

export function usePresenter() {
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
  const [nativeSources, setNativeSources] = useState<{ id: string; title: string; displayId: number }[]>([]);
  const [selectedSourceId, setSelectedSourceId] = useState<string>('');
  const [isSharing, setIsSharing] = useState<boolean>(false);
  const [copied, setCopied] = useState<'link' | 'code' | null>(null);
  const [previewStream, setPreviewStream] = useState<MediaStream | null>(null);
  const [spectatorCount, setSpectatorCount] = useState(0);
  const [captureContext, setCaptureContext] = useState<CaptureContext | null>(null);
  const [autoDetectFailed, setAutoDetectFailed] = useState(false);
  const [telemetry, setTelemetry] = useState<StreamTelemetry>(idleTelemetry());
  const { toasts, push: pushToast, dismiss: dismissToast } = useToasts();

  const [streamSettingsOpen, setStreamSettingsOpen] = useState(false);
  const [streamFps, setStreamFps] = useState(DEFAULT_STREAM_SETTINGS.fps);
  const [bitrateLimit, setBitrateLimit] = useState(DEFAULT_STREAM_SETTINGS.bitrateLimit);
  const [videoCodec, setVideoCodec] = useState<VideoCodec>(DEFAULT_STREAM_SETTINGS.videoCodec);
  const [resolution, setResolution] = useState<ResolutionPreset>(DEFAULT_STREAM_SETTINGS.resolution);

  const isSharingRef = useRef(false);
  const roomCodeRef = useRef('');
  const previewVideoRef = useRef<HTMLVideoElement | null>(null);
  const telemetryPollRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const broadcastStartRef = useRef<number | null>(null);
  const bitrateHistoryRef = useRef<number[]>([]);
  const streamFpsRef = useRef(DEFAULT_STREAM_SETTINGS.fps);
  const bitrateLimitRef = useRef(DEFAULT_STREAM_SETTINGS.bitrateLimit);
  const resolutionRef = useRef(DEFAULT_STREAM_SETTINGS.resolution);
  const videoCodecRef = useRef(DEFAULT_STREAM_SETTINGS.videoCodec);
  const audioAppIdRef = useRef<number | null>(null);
  const settingsHydratedRef = useRef(false);
  const lastSavedSettingsRef = useRef<StreamSettings | null>(null);

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
    videoCodecRef.current = videoCodec;
  }, [videoCodec]);

  useEffect(() => {
    audioAppIdRef.current = selectedAudioAppId;
  }, [selectedAudioAppId]);

  useEffect(() => {
    if (!settingsHydratedRef.current) return;
    const current = {
      fps: streamFps,
      bitrateLimit,
      videoCodec,
      resolution,
      apiEndpoint,
    };
    const last = lastSavedSettingsRef.current;
    if (last && streamSettingsEqual(last, current)) return;

    const timer = setTimeout(() => {
      void window.electronAPI
        ?.saveStreamSettings(current)
        .then((ok) => {
          if (!ok) {
            pushToast({
              title: 'Settings save failed',
              description: 'The settings file could not be written to disk.',
              variant: 'error',
            });
            return;
          }
          lastSavedSettingsRef.current = current;
          pushToast({ title: 'Stream settings saved', variant: 'success' });
        })
        .catch((err: unknown) => {
          console.error('[Presenter] saveStreamSettings IPC failed:', err);
        });
    }, SETTINGS_SAVE_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [streamFps, bitrateLimit, videoCodec, resolution, apiEndpoint, pushToast]);

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
        const switched = await window.electronAPI?.switchAudioCapture(selectedAudioAppId);
        if (!switched) {
          throw new Error('Native audio target switch failed');
        }
        audioAppIdRef.current = selectedAudioAppId;
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
  }, [selectedAudioAppId, isSharing, pushToast]);

  // Preview video binding — uses a canvas placeholder since video is
  // published natively by the Rust module, not by the renderer.
  useEffect(() => {
    const el = previewVideoRef.current;
    if (!el) return;
    el.srcObject = previewStream;
    if (previewStream) {
      el.play().catch(() => console.warn('Video autoplay blocked until user gesture'));
    }
  }, [previewStream]);

  // Data loading callbacks
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

  const loadNativeSources = useCallback(async () => {
    if (window.electronAPI) {
      try {
        const sources = await window.electronAPI.listScreenSources();
        setNativeSources(sources);
      } catch (err) {
        console.error('[Presenter] listScreenSources failed:', err);
      }
    }
  }, []);

  // Init effect
  useEffect(() => {
    (async () => {
      if (window.electronAPI) {
        const info = await window.electronAPI.getPlatformInfo();
        setIsWayland(info.isWayland);

        if (!info.isWayland) {
          loadDesktopSources();
          loadNativeSources();
        }

        const config = await window.electronAPI.getAppConfig();
        if (config.apiEndpoint) setApiEndpoint(config.apiEndpoint);
        if (config.livekitUrl) setLivekitUrl(config.livekitUrl);

        const saved = await window.electronAPI.getStreamSettings();
        lastSavedSettingsRef.current = saved;
        setStreamFps(saved.fps);
        setBitrateLimit(saved.bitrateLimit);
        setVideoCodec(saved.videoCodec);
        setResolution(saved.resolution);
        setApiEndpoint(saved.apiEndpoint);
        settingsHydratedRef.current = true;
      }
      loadAudioApps();
    })();

    return () => {
      if (telemetryPollRef.current) {
        clearInterval(telemetryPollRef.current);
        telemetryPollRef.current = null;
      }
    };
  }, [loadAudioApps, loadDesktopSources, loadNativeSources]);

  // Audio apps poll
  useEffect(() => {
    const interval = setInterval(() => {
      void loadAudioApps();
    }, AUDIO_APPS_POLL_MS);
    return () => clearInterval(interval);
  }, [loadAudioApps]);

  // Audio metering
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

    let app = await window.electronAPI.resolveAudioSource(opts ?? {});

    if (!app && opts?.nameHint) {
      app = await window.electronAPI.resolveAudioAppByName(opts.nameHint);
    }

    if (!app && audioApps.length === 1) {
      app = audioApps[0];
    }

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

  // Simplified telemetry — polls native connection status and spectator count.
  const startTelemetryPolling = () => {
    if (telemetryPollRef.current) return;
    broadcastStartRef.current = performance.now();
    bitrateHistoryRef.current = [];

    telemetryPollRef.current = setInterval(() => {
      const elapsedMs = broadcastStartRef.current ? performance.now() - broadcastStartRef.current : 0;
      const codecLabel = VIDEO_CODEC_LABEL_LK[videoCodecRef.current] ?? videoCodecRef.current;

      setTelemetry((p) => ({
        live: true,
        updatedAt: Date.now(),
        videoCodec: codecLabel,
        videoEncoder: null,
        width: null,
        height: null,
        targetFrameRate: streamFpsRef.current,
        frameRate: null,
        videoBitrate: null,
        audioCodec: 'Opus',
        audioBitrate: null,
        hasAudio: audioAppIdRef.current !== null,
        packetLossPct: null,
        roundTripTimeMs: null,
        bitrateHistory: bitrateHistoryRef.current,
        elapsedMs,
        spectatorCount: p.spectatorCount,
      }));
      void window.electronAPI?.getSpectatorCount().then((cnt) => {
        setTelemetry((prev) => ({ ...prev, spectatorCount: cnt }));
      });
    }, STATS_POLL_MS);
  };

  // Room management
  const handleCreateRoom = async () => {
    primeAudioContext();

    setAudioAppExplicitlySet(false);
    setAutoDetectedApp(null);
    setSelectedAudioAppId(null);
    setAutoDetectFailed(false);

    try {
      const res = await fetch(`${apiEndpoint}/api/rooms`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'X-Client-Origin': 'desktop' },
      });
      if (!res.ok) {
        const err = (await res.json().catch(() => ({ error: 'Unknown server error' }))) as { error?: string };
        throw new Error(err.error ?? `Server returned ${res.status}`);
      }
      const room = (await res.json()) as {
        code: string;
        shareUrl: string;
        token: string;
        livekitUrl: string;
        nativeToken?: string;
      };
      const code = room.code;
      const url = room.shareUrl;
      const resolvedLivekitUrl = livekitUrl || room.livekitUrl;
      const nativeToken = room.nativeToken ?? room.token;

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

      if (window.electronAPI) {
        await window.electronAPI.connectNativeRoom(resolvedLivekitUrl, nativeToken);
        console.log('[Presenter] Native LiveKit room connected');
      }
    } catch (err) {
      console.error('Failed to create room:', err);
      const message = err instanceof Error ? err.message : 'Failed to create room';
      pushToast({ title: 'Room creation failed', description: message, variant: 'error' });
    }
  };

  // Native video capture — the Rust module handles DesktopCapturer,
  // encoding, and publishing. The renderer creates a canvas placeholder
  // for the preview element.
  const startNativeVideo = async (): Promise<number> => {
    const dims = RESOLUTION_DIMENSIONS[resolutionRef.current];
    const config = {
      fps: streamFpsRef.current,
      width: dims.width,
      height: dims.height,
      videoCodec: videoCodecRef.current,
    };

    if (!window.electronAPI?.startNativeCapture) {
      throw new Error('Native capture not available on this platform');
    }

    // On Wayland, pass a negative index — DesktopCapturer invokes the portal.
    // Otherwise, use the selected source index from native source listing.
    const sourceIndex = isWayland ? -1 : nativeSources.findIndex((s) => s.id === selectedSourceId);
    const result = await window.electronAPI.startNativeCapture(sourceIndex >= 0 ? sourceIndex : 0, config);
    if (!result.ok) {
      throw new Error(result.error ?? 'Native video capture failed to start');
    }

    // Create a canvas placeholder for the preview element since video
    // is published natively.
    const canvas = document.createElement('canvas');
    canvas.width = dims.width;
    canvas.height = dims.height;
    const ctx = canvas.getContext('2d');
    if (ctx) {
      ctx.fillStyle = '#090d16';
      ctx.fillRect(0, 0, dims.width, dims.height);
      ctx.fillStyle = '#52525b';
      ctx.font = '16px monospace';
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';
      ctx.fillText('Video via native capture engine', dims.width / 2, dims.height / 2);
    }
    return canvas.captureStream(1).getVideoTracks()[0].id as unknown as number;
  };

  // Audio setup — starts native audio capture for the selected app.
  const setupAudioAsync = async (): Promise<void> => {
    let targetAudioId: number | null = selectedAudioAppId;

    if (targetAudioId === null && !audioAppExplicitlySet) {
      let app = await attemptAutoResolve(isWayland ? {} : { sourceId: selectedSourceId });

      if (!app && isWayland) {
        app = await attemptAutoResolve({ nameHint: nativeSources[0]?.title });
      }

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
      const isKdeOrUnknown = isWayland && (ctx?.de === 'kde' || ctx?.de === 'unknown');

      if (isMonitor || isKdeOrUnknown) {
        setAutoDetectFailed(false);
        targetAudioId = -1;
        pushToast({
          title: 'System audio',
          description: 'No app matched — capturing all desktop audio.',
          variant: 'info',
        });
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

    if (targetAudioId === null) return;

    try {
      if (window.electronAPI) {
        await window.electronAPI.startAudioCapture(targetAudioId);
      }
      audioAppIdRef.current = targetAudioId;
    } catch (err) {
      console.error('Audio capture failed (continuing video-only):', err);
      pushToast({
        title: 'Audio unavailable',
        description: 'Sharing video only — the selected audio source could not be captured.',
        variant: 'info',
      });
    }
  };

  const handleStartShare = async () => {
    primeAudioContext();
    try {
      await startNativeVideo();

      const dims = RESOLUTION_DIMENSIONS[resolutionRef.current];
      const canvas = document.createElement('canvas');
      canvas.width = dims.width;
      canvas.height = dims.height;
      const ctx = canvas.getContext('2d');
      if (ctx) {
        ctx.fillStyle = '#090d16';
        ctx.fillRect(0, 0, dims.width, dims.height);
        ctx.fillStyle = '#52525b';
        ctx.font = '16px monospace';
        ctx.textAlign = 'center';
        ctx.textBaseline = 'middle';
        ctx.fillText('Video via native capture engine', dims.width / 2, dims.height / 2);
      }
      const stream = canvas.captureStream(1);
      setPreviewStream(stream);

      setIsSharing(true);
      isSharingRef.current = true;

      setTelemetry({ ...idleTelemetry(), live: true });
      startTelemetryPolling();

      void setupAudioAsync().catch((err: unknown) => {
        console.error('[Presenter] Audio setup (background):', err);
      });
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
    setPreviewStream(null);

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
      if (window.electronAPI.stopNativeCapture) {
        await window.electronAPI.stopNativeCapture();
      }
      window.electronAPI.disconnectNativeRoom().catch((err: unknown) => {
        console.warn('[Presenter] disconnect native room:', err);
      });
    }

    setIsSharing(false);
    setSelectedAudioAppId(null);
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

  const canStartShare = !!roomCode && !isSharing;
  const startDisabledReason = (): string | null => {
    if (isSharing || canStartShare) return null;
    if (!roomCode) return 'Create a live room to start sharing.';
    return 'Select a window above to start sharing.';
  };
  const disabledReason = startDisabledReason();

  const shareButtonClass = ((): string => {
    if (isSharing) {
      return 'bg-destructive/90 hover:bg-destructive text-white';
    }
    return 'bg-safelight hover:bg-safelight-hover text-safelight-foreground';
  })();

  return {
    roomCode,
    shareUrl,
    apiEndpoint,
    livekitUrl,
    isWayland,
    audioApps,
    audioLevels,
    audioAppGroups,
    selectedAudioAppId,
    audioAppExplicitlySet,
    autoDetectedApp,
    desktopSources,
    selectedSourceId,
    isSharing,
    copied,
    previewStream,
    spectatorCount,
    captureContext,
    autoDetectFailed,
    telemetry,
    toasts,
    streamSettingsOpen,
    streamFps,
    bitrateLimit,
    videoCodec,
    resolution,
    setApiEndpoint,
    setSelectedAudioAppId,
    setAudioAppExplicitlySet,
    setAutoDetectedApp,
    setSelectedSourceId,
    setStreamSettingsOpen,
    setStreamFps,
    setBitrateLimit,
    setVideoCodec,
    setResolution,
    setCaptureContext,
    loadAudioApps,
    attemptAutoResolve,
    handleCreateRoom,
    handleStartShare,
    handleStopShare,
    handleCopyLink,
    handleCopyCode,
    previewVideoRef,
    canStartShare,
    disabledReason,
    shareButtonClass,
    pushToast,
    dismissToast,
  };
}
