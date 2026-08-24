import type { VideoCodec } from '@slopcast/shared-types';
import { codecLabel, RESOLUTION_DIMENSIONS } from '@slopcast/shared-types';
import type React from 'react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { createRoot } from 'react-dom/client';
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog';
import { Toaster } from '@/components/ui/sonner';
import { desktopApi } from './api/desktop';
import { AudioAppPicker } from './components/audio/AudioAppPicker';
import { PlatformNotice } from './components/gate/PlatformNotice';
import { TitleBar } from './components/layout/TitleBar';
import { ScreensharePreview } from './components/onboarding/preview/ScreensharePreview';
import { WelcomeBanner } from './components/onboarding/WelcomeBanner';
import type { ApiEndpointAvailability } from './components/settings/ApiEndpointField';
import { StreamSettingsPanel } from './components/settings/StreamSettingsPanel';
import { SourcePicker } from './components/sources/SourcePicker';
import { idleTelemetry } from './components/telemetry/StreamTelemetryBar';
import { useAudioCapture } from './hooks/useAudioCapture';
import type { PreparedRoom } from './hooks/useNativeRoom';
import { useNativeRoom } from './hooks/useNativeRoom';
import { useStreamSettings } from './hooks/useStreamSettings';
import { useStreamTelemetry } from './hooks/useStreamTelemetry';
import { notify, primeAudioContext } from './lib/toast';
import type { CaptureSourceSelection, CaptureStage, DesktopCaptureConfig, PlatformInfo, PreviewFrame } from './types';
import { recommendBitrateCap } from './utils/bitrate';
import { copyText } from './utils/clipboard';
import { codecOptionSuffix } from './utils/codecs';
import './index.css';

const ROOM_RETRY_MS = 45_000;

function parsePreviewPayload(payload: ArrayBuffer): PreviewFrame | null {
  if (!(payload instanceof ArrayBuffer)) return null;
  if (payload.byteLength < 16) return null;
  const view = new DataView(payload);
  let ptsUs = 0;
  let width = 0;
  let height = 0;
  try {
    ptsUs = Number(view.getBigUint64(0, true));
    width = view.getUint32(8, true);
    height = view.getUint32(12, true);
  } catch {
    return null;
  }
  if (width === 0 || height === 0) return null;
  return { ptsUs, width, height, data: new Uint8Array(payload, 16) };
}

async function fetchAndRender(
  lastPts: number,
  onNewFrame: (pts: number) => void,
  renderFrame: (frame: PreviewFrame) => void,
): Promise<void> {
  try {
    const resp = await fetch(`http://frame.localhost/frame.bin?t=${Date.now()}`);
    if (!resp.ok) return;
    const buf = await resp.arrayBuffer();
    if (buf.byteLength <= 16) return;
    const frame = parsePreviewPayload(buf);
    if (!frame || frame.ptsUs === lastPts) return;
    onNewFrame(frame.ptsUs);
    renderFrame(frame);
  } catch (err) {
    console.debug('[preview] frame fetch failed:', err);
  }
}

async function logLiveAudioSources(): Promise<void> {
  const sources = await desktopApi.dumpAudioSources();
  console.log(`[Presenter] live audio sources: ${sources.length}`);
  for (const source of sources) {
    console.log('[Presenter] audio source props:', JSON.stringify(source, null, 2));
  }
}

async function logSelectedApplication(label: string | null): Promise<void> {
  const context = await desktopApi.inspectCaptureContext();
  console.log(`[Presenter] selected application (trackLabel="${label}"):`, JSON.stringify(context, null, 2));
}

export const PresenterApp: React.FC = () => {
  const [platformInfo, setPlatformInfo] = useState<PlatformInfo | null>(null);
  const [captureStage, setCaptureStage] = useState<CaptureStage>('idle');
  const [previewFrame, setPreviewFrame] = useState<PreviewFrame | null>(null);
  const [showStopConfirm, setShowStopConfirm] = useState(false);
  const [copied, setCopied] = useState<'link' | 'code' | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [endpointAvailability, setEndpointAvailability] = useState<ApiEndpointAvailability>('checking');
  const [hasHealthyApiEndpoint, setHasHealthyApiEndpoint] = useState(false);
  const [endpointSwitchCandidate, setEndpointSwitchCandidate] = useState<string | null>(null);
  const [endpointValidationRevision, setEndpointValidationRevision] = useState(0);
  const [isRoomTransitioning, setIsRoomTransitioning] = useState(false);
  const [retryableReplacement, setRetryableReplacement] = useState<{
    expiresAt: number;
    room: PreparedRoom;
    shouldResumeSharing: boolean;
  } | null>(null);
  const selectedCaptureSourceRef = useRef<CaptureSourceSelection | null>(null);
  const isConfirmingEndpointSwitchRef = useRef(false);
  const hadAudioCaptureRef = useRef(false);

  const {
    apiEndpoint,
    activateApiEndpoint,
    pendingApiEndpoint,
    setPendingApiEndpoint,
    livekitUrl,
    streamSettingsOpen,
    setStreamSettingsOpen,
    streamFps,
    setStreamFps,
    bitrateLimit,
    setBitrateLimit,
    availableCodecs,
    videoCodec,
    setVideoCodec,
    resolution,
    setResolution,
    autoBitrate,
    setAutoBitrate,
    motionMode,
    setMotionMode,
    streamFpsRef,
    resolutionRef,
    autoBitrateRef,
    settingsHydrated,
  } = useStreamSettings();

  const handleRoomDisconnect = useCallback((): void => {
    setCaptureStage('idle');
    setPreviewFrame(null);
    setEndpointValidationRevision((revision) => revision + 1);
    if (!hadAudioCaptureRef.current) return;

    hadAudioCaptureRef.current = false;
    void desktopApi.stopAudioCapture().then((stopped) => {
      if (!stopped) notify('error', 'Audio capture cleanup failed', 'Stop the audio capture before sharing again.');
    });
  }, []);

  const {
    roomCode,
    shareUrl,
    spectatorCount,
    isCreatingRoom,
    isClosingRoom,
    createRoom: createNativeRoom,
    prepareRoom,
    connectPreparedRoom,
    discardPreparedRoom,
    closeRoom,
    closeRoomBestEffort,
    roomEndpoint,
  } = useNativeRoom({
    apiEndpoint,
    livekitUrl,
    onDisconnect: handleRoomDisconnect,
  });

  useEffect(() => {
    if (!retryableReplacement) return;
    const remainingMs = Math.max(0, retryableReplacement.expiresAt - Date.now());
    const timer = setTimeout(() => {
      void discardPreparedRoom(retryableReplacement.room);
      setRetryableReplacement(null);
      setEndpointValidationRevision((revision) => revision + 1);
      notify('info', 'Replacement room expired', 'Create a fresh room when you are ready to try again.');
    }, remainingMs);
    return () => clearTimeout(timer);
  }, [discardPreparedRoom, retryableReplacement]);

  const {
    audioApps,
    audioAppGroups,
    selectedAudioAppId,
    setSelectedAudioAppId,
    audioAppExplicitlySet,
    setAudioAppExplicitlySet,
    autoDetectedApp,
    setAutoDetectedApp,
    autoDetectFailed,
    setAutoDetectFailed,
    captureContext,
    setCaptureContext,
    audioAppIdRef,
    loadAudioApps,
    startAudioCapture,
    switchAudioCapture,
    attemptAutoResolve,
    handleSelectApp,
  } = useAudioCapture(captureStage === 'live');
  hadAudioCaptureRef.current = audioAppIdRef.current !== null;

  const { telemetry, setTelemetry, startTelemetryPolling, stopTelemetryPolling, resetStatsPrev } =
    useStreamTelemetry(spectatorCount);

  const activeVideoCodecRef = useRef<VideoCodec>(videoCodec);
  const captureSessionRef = useRef(0);
  const lastVideoConfigKeyRef = useRef<string | null>(null);

  const effectiveBitrate = useMemo(
    () =>
      autoBitrate
        ? recommendBitrateCap({
            codec: videoCodec,
            resolution,
            fps: streamFps,
            motionTier: 'static',
          })
        : bitrateLimit,
    [autoBitrate, videoCodec, resolution, streamFps, bitrateLimit],
  );
  const effectiveBitrateRef = useRef(effectiveBitrate);
  useEffect(() => {
    effectiveBitrateRef.current = effectiveBitrate;
  }, [effectiveBitrate]);

  const videoConfigKey = useCallback((): string => {
    const dims = RESOLUTION_DIMENSIONS[resolutionRef.current];
    return JSON.stringify({
      fps: streamFpsRef.current,
      bitrate: effectiveBitrateRef.current,
      auto: autoBitrateRef.current,
      codec: videoCodec,
      width: dims.width,
      height: dims.height,
    });
  }, [videoCodec, resolutionRef, streamFpsRef, autoBitrateRef]);

  const buildCaptureConfig = useCallback((): DesktopCaptureConfig => {
    const dims = RESOLUTION_DIMENSIONS[resolutionRef.current];
    return {
      fps: streamFpsRef.current,
      width: dims.width,
      height: dims.height,
      videoCodec,
      maxBitrate: effectiveBitrateRef.current,
      autoBitrate: autoBitrateRef.current,
    };
  }, [resolutionRef, streamFpsRef, videoCodec, autoBitrateRef]);

  useEffect(() => {
    (async () => {
      const info = await desktopApi.getPlatformInfo();
      setPlatformInfo(info);
    })();

    let disposed = false;
    if (window.__PREVIEW_BENCH__) {
      window.__PREVIEW_BENCH_DATA__ = [];
    }
    const pollFrame = (): void => {
      let lastPts = 0;
      const poll = async (): Promise<void> => {
        if (!disposed)
          await fetchAndRender(
            lastPts,
            (pts) => {
              lastPts = pts;
            },
            setPreviewFrame,
          );
        if (!disposed) requestAnimationFrame(poll);
      };
      requestAnimationFrame(poll);
    };
    pollFrame();

    return () => {
      disposed = true;
      closeRoomBestEffort();
    };
  }, [closeRoomBestEffort]);

  useEffect(() => {
    if (captureStage !== 'live') {
      lastVideoConfigKeyRef.current = null;
      return;
    }

    const dims = RESOLUTION_DIMENSIONS[resolution];
    const key = JSON.stringify({
      fps: streamFps,
      bitrate: effectiveBitrate,
      auto: autoBitrate,
      codec: videoCodec,
      width: dims.width,
      height: dims.height,
    });
    if (lastVideoConfigKeyRef.current === key) return;

    const prevCodec = activeVideoCodecRef.current;
    const session = captureSessionRef.current;
    const timeout = setTimeout(() => {
      void desktopApi
        .updateNativeVideo({
          fps: streamFps,
          width: dims.width,
          height: dims.height,
          videoCodec,
          maxBitrate: effectiveBitrate,
          autoBitrate,
        })
        .then((ok) => {
          if (!ok || captureSessionRef.current !== session) return;
          lastVideoConfigKeyRef.current = key;
          resetStatsPrev();
          activeVideoCodecRef.current = videoCodec;
          console.log(
            `[Presenter] Live encoder update: codec=${videoCodec} fps=${streamFps} bitrate=${(effectiveBitrate / 1_000_000).toFixed(0)}Mbps`,
          );
          if (prevCodec !== videoCodec) {
            notify(
              'info',
              'Video codec updated',
              `Switched video codec to ${codecLabel(`VIDEO/${videoCodec.toUpperCase()}`) ?? videoCodec.toUpperCase()}`,
            );
          }
        });
    }, 300);
    return () => clearTimeout(timeout);
  }, [streamFps, effectiveBitrate, videoCodec, resolution, autoBitrate, captureStage, resetStatsPrev]);

  const handleCreateRoom = useCallback(async () => {
    setAudioAppExplicitlySet(false);
    setAutoDetectedApp(null);
    setSelectedAudioAppId(null);
    setAutoDetectFailed(false);

    await createNativeRoom();
  }, [setAudioAppExplicitlySet, setAutoDetectedApp, setSelectedAudioAppId, setAutoDetectFailed, createNativeRoom]);

  const resetCaptureState = useCallback((): void => {
    captureSessionRef.current += 1;
    lastVideoConfigKeyRef.current = null;
    setCaptureStage('idle');
    stopTelemetryPolling();
    audioAppIdRef.current = null;
    setPreviewFrame(null);
    if (!audioAppExplicitlySet) setSelectedAudioAppId(null);
    setAudioAppExplicitlySet(false);
    setAutoDetectedApp(null);
    setAutoDetectFailed(false);
  }, [
    stopTelemetryPolling,
    audioAppIdRef,
    audioAppExplicitlySet,
    setSelectedAudioAppId,
    setAudioAppExplicitlySet,
    setAutoDetectedApp,
    setAutoDetectFailed,
  ]);

  const stopCaptureAfterRoomClosure = useCallback(async (): Promise<void> => {
    const hadAudioCapture = audioAppIdRef.current !== null;
    resetCaptureState();
    hadAudioCaptureRef.current = false;
    if (!hadAudioCapture) return;

    const stopped = await desktopApi.stopAudioCapture();
    if (!stopped) {
      notify('error', 'Audio capture cleanup failed', 'The room closed, but audio capture did not stop cleanly.');
    }
  }, [audioAppIdRef, resetCaptureState]);

  const handleStopShare = useCallback(async () => {
    resetCaptureState();
    const stopped = await desktopApi.stopNativeCapture();
    if (!stopped) {
      notify('error', 'Screenshare stop failed', 'The room remains open, but capture could not be stopped cleanly.');
    }
  }, [resetCaptureState]);

  useEffect(() => {
    const unlistenPromise = desktopApi.onCaptureEnded(() => {
      notify('info', 'Stream ended', 'The captured window was closed, so sharing stopped.');
      captureSessionRef.current += 1;
      lastVideoConfigKeyRef.current = null;
      setCaptureStage('idle');
      stopTelemetryPolling();
      setPreviewFrame(null);
      audioAppIdRef.current = null;
      setSelectedAudioAppId(null);
      setAudioAppExplicitlySet(false);
      setAutoDetectedApp(null);
      void desktopApi.stopNativeCapture().then((stopped) => {
        if (!stopped) {
          notify('error', 'Capture stop failed', 'The room remains open, but capture could not be stopped cleanly.');
        }
      });
    });
    return () => {
      void unlistenPromise.then((unlisten) => unlisten());
    };
  }, [audioAppIdRef, setAudioAppExplicitlySet, setAutoDetectedApp, setSelectedAudioAppId, stopTelemetryPolling]);

  const resolveSystemAudioFallback = useCallback(async (): Promise<boolean> => {
    setAutoDetectFailed(true);
    const ctx = await desktopApi.getCaptureContext();
    setCaptureContext(ctx);

    const canUseDesktopAudio = platformInfo?.platform === 'windows' || ctx?.sourceType === 'monitor';
    if (!canUseDesktopAudio) {
      notify('info', 'No audio detected', 'Sharing video only. Select an audio app and restart to include audio.');
      return false;
    }

    setAutoDetectFailed(false);
    if (!audioAppExplicitlySet) {
      setSelectedAudioAppId(-1);
      setAutoDetectedApp({ id: -1, name: 'Desktop Audio', processId: 0 });
    }
    console.log('[Presenter] No specific app resolved — using system audio (desktop audio fallback)');
    return true;
  }, [
    audioAppExplicitlySet,
    platformInfo?.platform,
    setAutoDetectFailed,
    setCaptureContext,
    setSelectedAudioAppId,
    setAutoDetectedApp,
  ]);

  const resolveAudioTarget = useCallback(async (): Promise<number | null> => {
    let targetAudioId: number | null = selectedAudioAppId;

    if (targetAudioId === null && !audioAppExplicitlySet) {
      await loadAudioApps();
      const resolved = await attemptAutoResolve();
      targetAudioId = resolved ? resolved.id : null;
    }

    if (targetAudioId !== null) {
      setAutoDetectFailed(false);
      return targetAudioId;
    }

    const usedSystemAudio = await resolveSystemAudioFallback();
    return usedSystemAudio ? -1 : null;
  }, [
    selectedAudioAppId,
    audioAppExplicitlySet,
    loadAudioApps,
    attemptAutoResolve,
    resolveSystemAudioFallback,
    setAutoDetectFailed,
  ]);

  const captureAudioForTarget = useCallback(
    async (targetAudioId: number | null): Promise<void> => {
      if (targetAudioId === null) return;
      try {
        if (audioAppIdRef.current === null) {
          await startAudioCapture(targetAudioId);
        } else if (audioAppIdRef.current !== targetAudioId) {
          await switchAudioCapture(targetAudioId);
        }
      } catch (err) {
        console.error('Audio capture failed (continuing video-only):', err);
        notify('info', 'Audio unavailable', 'Sharing video only — the selected audio source could not be captured.');
      }
    },
    [audioAppIdRef, startAudioCapture, switchAudioCapture],
  );

  const cleanupFailedShare = useCallback(async (): Promise<void> => {
    await desktopApi.stopNativeCapture();
    await desktopApi.stopAudioCapture();
    if (!audioAppExplicitlySet) {
      setSelectedAudioAppId(null);
      setAutoDetectedApp(null);
    }
  }, [audioAppExplicitlySet, setSelectedAudioAppId, setAutoDetectedApp]);

  const getTelemetryInputs = useCallback(() => {
    const dims = RESOLUTION_DIMENSIONS[resolutionRef.current];
    return {
      width: dims.width,
      height: dims.height,
      targetFrameRate: streamFpsRef.current,
      hasAudio: audioAppIdRef.current != null,
    };
  }, [resolutionRef, streamFpsRef, audioAppIdRef]);

  const activateLive = useCallback(
    async (session: number): Promise<void> => {
      if (captureSessionRef.current !== session) return;
      const targetAudioId = await resolveAudioTarget();
      if (captureSessionRef.current !== session) return;
      await captureAudioForTarget(targetAudioId);
      if (captureSessionRef.current !== session) return;
      activeVideoCodecRef.current = videoCodec;
      lastVideoConfigKeyRef.current = videoConfigKey();
      setCaptureStage('live');
      setTelemetry({ ...idleTelemetry(), live: true });
      notify('success', 'Stream started', 'Your screen is now live.');
      startTelemetryPolling(getTelemetryInputs);
      if (import.meta.env.DEV || import.meta.env.VITE_E2E === '1') {
        void logLiveAudioSources();
      }
      void logSelectedApplication(null);
    },
    [
      resolveAudioTarget,
      captureAudioForTarget,
      videoCodec,
      videoConfigKey,
      setTelemetry,
      startTelemetryPolling,
      getTelemetryInputs,
    ],
  );

  const startCombinedShare = useCallback(
    async (source?: CaptureSourceSelection): Promise<void> => {
      const session = captureSessionRef.current + 1;
      captureSessionRef.current = session;
      const res = await desktopApi.startNativeCapture(buildCaptureConfig(), source);
      if (!res.ok) {
        throw new Error(res.error ?? 'Native capture failed to start');
      }
      await activateLive(session);
    },
    [buildCaptureConfig, activateLive],
  );

  const startPreviewCapture = useCallback(
    async (source?: CaptureSourceSelection): Promise<void> => {
      primeAudioContext();
      try {
        const previewStarted = await desktopApi.startCapturePreview(source);
        if (previewStarted) {
          setPreviewFrame(null);
          setCaptureStage('previewing');
          return;
        }
        await startCombinedShare(source);
      } catch (err: unknown) {
        console.error('Failed to capture screen:', err);
        const message = err instanceof Error ? err.message : 'Unknown capture error';
        notify('error', 'Screenshare failed to start', message);
        setCaptureStage('idle');
        await cleanupFailedShare();
      }
    },
    [startCombinedShare, cleanupFailedShare],
  );

  const handleStartShare = useCallback(async () => {
    if (platformInfo?.platform === 'windows') {
      setPickerOpen(true);
      return;
    }
    await startPreviewCapture();
  }, [platformInfo, startPreviewCapture]);

  const handleSourceSelected = useCallback(
    (selection: CaptureSourceSelection): void => {
      selectedCaptureSourceRef.current = selection;
      setPickerOpen(false);
      void startPreviewCapture(selection);
    },
    [startPreviewCapture],
  );

  const handleGoLive = useCallback(async () => {
    primeAudioContext();
    const session = captureSessionRef.current + 1;
    captureSessionRef.current = session;
    try {
      const config = buildCaptureConfig();
      const source = selectedCaptureSourceRef.current ?? undefined;
      const published = await desktopApi.goLive(config, source);
      if (!published) {
        const res = await desktopApi.startNativeCapture(config, source);
        if (!res.ok) {
          throw new Error(res.error ?? 'Native capture failed to start');
        }
      }
      await activateLive(session);
    } catch (err: unknown) {
      console.error('Failed to go live:', err);
      const message = err instanceof Error ? err.message : 'Unknown capture error';
      notify('error', 'Go live failed', message);
    }
  }, [buildCaptureConfig, activateLive]);

  const resumeSharingAfterReplacement = useCallback(
    async (shouldResumeSharing: boolean): Promise<void> => {
      if (!shouldResumeSharing) return;
      try {
        const source = selectedCaptureSourceRef.current ?? undefined;
        await startCombinedShare(source);
      } catch (error) {
        console.error('Failed to resume sharing after room replacement:', error);
        await cleanupFailedShare();
        setCaptureStage('idle');
        const message = error instanceof Error ? error.message : 'The capture source could not be restarted.';
        notify('error', 'Sharing did not resume', `${message} The new room remains open.`);
      }
    },
    [cleanupFailedShare, startCombinedShare],
  );

  const handleApiEndpointAvailabilityChange = useCallback(
    (availability: ApiEndpointAvailability, endpoint: string): void => {
      setEndpointAvailability(availability);
      if (endpoint === apiEndpoint) setHasHealthyApiEndpoint(availability === 'healthy');
    },
    [apiEndpoint],
  );

  const handleApiEndpointValidated = useCallback(
    (endpoint: string): void => {
      if (endpoint === apiEndpoint) {
        setHasHealthyApiEndpoint(true);
        if (!roomCode) setPendingApiEndpoint(null);
        return;
      }
      if (endpoint === pendingApiEndpoint && roomCode) return;
      if (!roomCode && !retryableReplacement) {
        activateApiEndpoint(endpoint);
        setHasHealthyApiEndpoint(true);
        return;
      }

      setEndpointSwitchCandidate(endpoint);
    },
    [activateApiEndpoint, apiEndpoint, pendingApiEndpoint, retryableReplacement, roomCode, setPendingApiEndpoint],
  );

  const handleApiEndpointReset = useCallback((): void => {
    setEndpointSwitchCandidate(null);
    setPendingApiEndpoint(null);
  }, [setPendingApiEndpoint]);

  const deferEndpointSwitch = useCallback((): void => {
    if (!endpointSwitchCandidate) return;
    setPendingApiEndpoint(endpointSwitchCandidate);
    setEndpointSwitchCandidate(null);
  }, [endpointSwitchCandidate, setPendingApiEndpoint]);

  const handleEndpointDialogOpenChange = useCallback(
    (open: boolean): void => {
      if (open || isConfirmingEndpointSwitchRef.current) return;
      deferEndpointSwitch();
    },
    [deferEndpointSwitch],
  );

  const handleReplaceRoom = useCallback(async (): Promise<void> => {
    const candidate = endpointSwitchCandidate;
    if (!candidate || isRoomTransitioning) return;
    isConfirmingEndpointSwitchRef.current = true;
    setIsRoomTransitioning(true);

    const shouldResumeSharing = captureStage === 'live';
    const replacement = await prepareRoom(candidate);
    if (!replacement) {
      setPendingApiEndpoint(candidate);
      setEndpointSwitchCandidate(null);
      isConfirmingEndpointSwitchRef.current = false;
      setIsRoomTransitioning(false);
      return;
    }

    const closed = await closeRoom();
    if (!closed.ok) {
      await discardPreparedRoom(replacement);
      setPendingApiEndpoint(candidate);
      setEndpointSwitchCandidate(null);
      isConfirmingEndpointSwitchRef.current = false;
      setIsRoomTransitioning(false);
      notify('error', 'Room replacement stopped', closed.error ?? 'The current room could not be closed.');
      return;
    }

    await stopCaptureAfterRoomClosure();
    const connected = await connectPreparedRoom(replacement);
    if (!connected.ok) {
      setRetryableReplacement({
        expiresAt: Date.now() + ROOM_RETRY_MS,
        room: replacement,
        shouldResumeSharing,
      });
      setPendingApiEndpoint(candidate);
      setEndpointSwitchCandidate(null);
      isConfirmingEndpointSwitchRef.current = false;
      setIsRoomTransitioning(false);
      notify('error', 'Replacement room did not connect', 'One retry is available in the room controls.');
      return;
    }

    activateApiEndpoint(candidate);
    setEndpointSwitchCandidate(null);
    isConfirmingEndpointSwitchRef.current = false;
    setIsRoomTransitioning(false);
    await resumeSharingAfterReplacement(shouldResumeSharing);
  }, [
    activateApiEndpoint,
    captureStage,
    closeRoom,
    connectPreparedRoom,
    discardPreparedRoom,
    endpointSwitchCandidate,
    isRoomTransitioning,
    prepareRoom,
    resumeSharingAfterReplacement,
    stopCaptureAfterRoomClosure,
    setPendingApiEndpoint,
  ]);

  const handleRetryRoomConnection = useCallback(async (): Promise<void> => {
    const replacement = retryableReplacement;
    if (!replacement || isRoomTransitioning) return;
    if (Date.now() >= replacement.expiresAt) {
      await discardPreparedRoom(replacement.room);
      setRetryableReplacement(null);
      setEndpointValidationRevision((revision) => revision + 1);
      notify('error', 'Replacement room expired', 'Create a fresh room to try again.');
      return;
    }
    setRetryableReplacement(null);
    setIsRoomTransitioning(true);

    const connected = await connectPreparedRoom(replacement.room);
    if (!connected.ok) {
      await discardPreparedRoom(replacement.room);
      setEndpointValidationRevision((revision) => revision + 1);
      setIsRoomTransitioning(false);
      notify('error', 'Room connection failed', connected.error ?? 'Create a fresh room to try again.');
      return;
    }

    activateApiEndpoint(replacement.room.apiEndpoint);
    setIsRoomTransitioning(false);
    await resumeSharingAfterReplacement(replacement.shouldResumeSharing);
  }, [
    activateApiEndpoint,
    connectPreparedRoom,
    discardPreparedRoom,
    isRoomTransitioning,
    resumeSharingAfterReplacement,
    retryableReplacement,
  ]);

  const handleCloseRoom = useCallback(async (): Promise<void> => {
    if (isRoomTransitioning) return;
    setIsRoomTransitioning(true);
    const result = await closeRoom();
    if (!result.ok) {
      setIsRoomTransitioning(false);
      notify('error', 'Room closure failed', result.error ?? 'The room remains open. Try again.');
      return;
    }

    await stopCaptureAfterRoomClosure();
    setIsRoomTransitioning(false);
    if (pendingApiEndpoint) setEndpointValidationRevision((revision) => revision + 1);
  }, [closeRoom, isRoomTransitioning, pendingApiEndpoint, stopCaptureAfterRoomClosure]);

  const flashCopied = useCallback((kind: 'link' | 'code') => {
    setCopied(kind);
    setTimeout(() => setCopied(null), 2000);
  }, []);

  const handleCopyLink = useCallback(async () => {
    const url = shareUrl;
    if (!url) return;
    const ok = await copyText(url);
    if (ok) {
      flashCopied('link');
    } else {
      notify('error', 'Copy failed', 'Room link could not be copied.');
    }
  }, [shareUrl, flashCopied]);

  const handleCopyCode = useCallback(async () => {
    if (!roomCode) return;
    const ok = await copyText(roomCode);
    if (ok) {
      flashCopied('code');
    } else {
      notify('error', 'Copy failed', 'Room code could not be copied.');
    }
  }, [roomCode, flashCopied]);

  const isEndpointCheckPending = endpointAvailability === 'checking' || endpointAvailability === 'typing';
  const canCreateRoom = settingsHydrated && hasHealthyApiEndpoint && !isEndpointCheckPending && !isRoomTransitioning;
  const roomCreateDisabledReason = (): string | null => {
    if (!settingsHydrated) return 'Loading saved API endpoint settings.';
    if (endpointAvailability === 'checking') return 'Checking the API endpoint before room creation.';
    if (endpointAvailability === 'typing') return 'Finish editing the API endpoint before creating a room.';
    if (hasHealthyApiEndpoint) return null;
    if (endpointAvailability === 'error') return 'Fix the API endpoint before creating a room.';
    return null;
  };
  const canStartShare = !!roomCode && captureStage === 'idle' && !isRoomTransitioning;
  const canGoLive = captureStage === 'previewing' && previewFrame !== null && !isRoomTransitioning;
  const startDisabledReason = (): string | null => {
    if (captureStage !== 'idle' || canStartShare) return null;
    if (!roomCode) return 'Create a live room to start sharing.';
    return null;
  };
  const disabledReason = startDisabledReason();

  let content: React.ReactNode = null;
  if (platformInfo && !platformInfo.videoCaptureAvailable) {
    content = (
      <div className="flex-1 overflow-y-auto">
        <PlatformNotice platform={platformInfo.platform} />
      </div>
    );
  } else if (platformInfo) {
    content = (
      <div className="flex-1 overflow-y-auto">
        <main className="max-w-5xl mx-auto w-full px-6 py-6 space-y-6">
          <WelcomeBanner />

          <ScreensharePreview
            captureStage={captureStage}
            roomCode={roomCode}
            copied={copied}
            previewFrame={previewFrame}
            telemetry={telemetry}
            onCopyLink={handleCopyLink}
          />

          <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
            <SourcePicker
              roomCode={roomCode}
              isCreatingRoom={isCreatingRoom}
              isClosingRoom={isClosingRoom}
              isRoomTransitioning={isRoomTransitioning}
              canCreateRoom={canCreateRoom}
              roomCreateDisabledReason={roomCreateDisabledReason()}
              hasRetryableRoom={retryableReplacement !== null}
              copied={copied}
              onCreateRoom={handleCreateRoom}
              onRetryRoomConnection={() => void handleRetryRoomConnection()}
              onCloseRoom={() => void handleCloseRoom()}
              onCopyCode={handleCopyCode}
              onCopyLink={handleCopyLink}
              captureContext={captureContext}
              autoDetectFailed={autoDetectFailed}
              captureStage={captureStage}
              showStopConfirm={showStopConfirm}
              setShowStopConfirm={setShowStopConfirm}
              spectatorCount={spectatorCount}
              canStartShare={canStartShare}
              canGoLive={canGoLive}
              disabledReason={disabledReason}
              pickerOpen={pickerOpen}
              setPickerOpen={setPickerOpen}
              onSourceSelected={handleSourceSelected}
              onStartShare={handleStartShare}
              onGoLive={handleGoLive}
              onStopShare={handleStopShare}
            />

            <AudioAppPicker
              audioApps={audioApps}
              audioAppGroups={audioAppGroups}
              selectedAudioAppId={selectedAudioAppId}
              autoDetectedApp={autoDetectedApp}
              onSelectApp={handleSelectApp}
              onRefresh={loadAudioApps}
            />
          </div>

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
            effectiveBitrate={effectiveBitrate}
            autoBitrate={autoBitrate}
            setAutoBitrate={setAutoBitrate}
            motionMode={motionMode}
            setMotionMode={setMotionMode}
            apiEndpoint={apiEndpoint}
            pendingApiEndpoint={pendingApiEndpoint}
            roomEndpoint={roomEndpoint}
            endpointValidationRevision={endpointValidationRevision}
            endpointControlsDisabled={isRoomTransitioning || retryableReplacement !== null}
            onApiEndpointAvailabilityChange={handleApiEndpointAvailabilityChange}
            onApiEndpointValidated={handleApiEndpointValidated}
            onApiEndpointReset={handleApiEndpointReset}
          />
        </main>
      </div>
    );
  }

  return (
    <div className="h-screen flex flex-col overflow-hidden">
      <TitleBar
        isLive={captureStage === 'live'}
        isPreviewing={captureStage === 'previewing'}
        onClose={closeRoomBestEffort}
      />
      {content}
      <AlertDialog open={endpointSwitchCandidate !== null} onOpenChange={handleEndpointDialogOpenChange}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>API endpoint changed</AlertDialogTitle>
            <AlertDialogDescription className="space-y-2 leading-relaxed">
              <span className="block">Recreating the room disconnects spectators and changes the room link.</span>
              {captureStage === 'live' && (
                <span className="block">
                  Slopcast will resume sharing, but your operating system may ask you to choose the screen or window
                  again.
                </span>
              )}
              {captureStage === 'previewing' && (
                <span className="block">The current preview will close. You can choose a source again afterward.</span>
              )}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={isRoomTransitioning}>Use after this room closes</AlertDialogCancel>
            <AlertDialogAction onClick={() => void handleReplaceRoom()} disabled={isRoomTransitioning}>
              {captureStage === 'live' ? 'Recreate room and resume sharing' : 'Recreate room'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      <Toaster />
    </div>
  );
};

const rootEl = document.getElementById('root');
if (!rootEl) throw new Error('Missing #root element');
const root = createRoot(rootEl);
root.render(<PresenterApp />);
