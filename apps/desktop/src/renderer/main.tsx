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
import { DesktopSettings } from './components/settings/DesktopSettings';
import { SourcePicker } from './components/sources/SourcePicker';
import { idleTelemetry } from './components/telemetry/StreamTelemetryBar';
import { useAudioCapture } from './hooks/useAudioCapture';
import { useDesktopPreferences } from './hooks/useDesktopPreferences';
import type { PreparedRoom } from './hooks/useNativeRoom';
import { useNativeRoom } from './hooks/useNativeRoom';
import { useStreamSettings } from './hooks/useStreamSettings';
import { useStreamTelemetry } from './hooks/useStreamTelemetry';
import { notify, primeAudioContext } from './lib/toast';
import type { CaptureSourceSelection, CaptureStage, DesktopCaptureConfig, PlatformInfo, PreviewFrame } from './types';
import type { DesktopAction } from './types/preferences';
import { checkApiEndpoint } from './utils/apiEndpoint';
import { toAudioTargetId } from './utils/audio-targets';
import { recommendBitrateCap } from './utils/bitrate';
import { copyText } from './utils/clipboard';
import './index.css';

const ROOM_RETRY_MS = 45_000;

function revealMainWindow(): void {
  void desktopApi.showMainWindow().catch((cause: unknown) => console.error('Cannot show Slopcast:', cause));
}

function matchesTelemetryShortcut(event: KeyboardEvent, chord: string): boolean {
  if (!chord) return false;

  const parts = chord.split('+');
  const key = parts.pop();
  const shortCode = event.code.replace(/^(Key|Digit)/, '');

  return (
    (key === event.code || key === shortCode) &&
    event.ctrlKey === parts.includes('Control') &&
    event.metaKey === parts.includes('Command') &&
    event.shiftKey === parts.includes('Shift') &&
    event.altKey === parts.includes('Alt')
  );
}

type ConfirmationAction = 'close-room' | 'quit' | 'quit-anyway';

function DesktopActionConfirmation({
  action,
  isBusy,
  onCancel,
  onConfirm,
}: {
  action: ConfirmationAction | null;
  isBusy: boolean;
  onCancel: () => void;
  onConfirm: () => Promise<void>;
}) {
  const previousFocusRef = useRef<HTMLElement | null>(null);
  if (!action) return null;

  const title = action === 'close-room' ? 'Close this room?' : 'Quit Slopcast?';
  const label = action === 'close-room' ? 'Close room' : 'Quit';
  let description = 'Sharing will stop and spectators will disconnect. Closing the room makes its link unusable.';
  if (action === 'quit-anyway')
    description =
      'The server could not close your room. Capture will stop, but the link may remain until the server expires it. Quit anyway?';

  return (
    <AlertDialog
      open
      onOpenChange={(open) => {
        if (!open) onCancel();
      }}
    >
      <AlertDialogContent
        onOpenAutoFocus={() => {
          const focused = document.activeElement;
          if (focused instanceof HTMLElement) previousFocusRef.current = focused;
        }}
        onCloseAutoFocus={(event) => {
          event.preventDefault();
          if (previousFocusRef.current?.isConnected) previousFocusRef.current.focus();
          else document.querySelector<HTMLButtonElement>('button[aria-label="Settings"]')?.focus();
        }}
      >
        <AlertDialogHeader>
          <AlertDialogTitle>{title}</AlertDialogTitle>
          <AlertDialogDescription>{description}</AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction
            disabled={isBusy}
            className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            onClick={() => void onConfirm().catch((cause: unknown) => notify('error', 'Action failed', String(cause)))}
          >
            {label}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

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
  const [showTelemetry, setShowTelemetry] = useState(true);
  const [showStopConfirm, setShowStopConfirm] = useState(false);
  const [copied, setCopied] = useState<'link' | 'code' | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [isCaptureBusy, setIsCaptureBusy] = useState(false);
  const [isMuted, setIsMuted] = useState(false);
  const [confirmation, setConfirmation] = useState<'close-room' | 'quit' | 'quit-anyway' | null>(null);
  const captureBusyRef = useRef<boolean>(false);
  const stopBusyRef = useRef<boolean>(false);
  const roomActionRef = useRef<boolean>(false);
  const muteBusyRef = useRef<boolean>(false);
  const { snapshot: preferencesSnapshot, error: preferencesError } = useDesktopPreferences();
  const autoSelectAudio = preferencesSnapshot?.preferences.autoSelectAudio ?? false;
  const telemetryShortcutRef = useRef('');
  telemetryShortcutRef.current = preferencesSnapshot?.preferences.telemetryShortcut ?? '';
  const roomCodeRef = useRef<string>('');
  const healthyEndpointRef = useRef<boolean>(false);
  const trayShareRef = useRef<boolean>(false);
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
  const captureSessionRef = useRef(0);
  const stopAudioCaptureRef = useRef(() => desktopApi.stopAudioCapture());

  const {
    apiEndpoint,
    activateApiEndpoint,
    pendingApiEndpoint,
    setPendingApiEndpoint,
    livekitUrl,
    streamFps,
    bitrateLimit,
    videoCodec,
    resolution,
    autoBitrate,
    streamFpsRef,
    resolutionRef,
    autoBitrateRef,
    settingsHydrated,
  } = useStreamSettings();

  const handleRoomDisconnect = useCallback((): void => {
    captureSessionRef.current += 1;
    setCaptureStage('idle');
    setPreviewFrame(null);
    setEndpointValidationRevision((revision) => revision + 1);
    void stopAudioCaptureRef.current().catch((cause: unknown) => {
      notify('error', 'Audio capture cleanup failed', String(cause));
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
  roomCodeRef.current = roomCode;
  healthyEndpointRef.current = hasHealthyApiEndpoint;

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
    stopAudioCapture,
    startAudioCapture,
    attemptAutoResolve,
    handleSelectApp,
  } = useAudioCapture(captureStage === 'live', platformInfo?.platform === 'linux', autoSelectAudio);
  stopAudioCaptureRef.current = stopAudioCapture;
  const audioChoiceRef = useRef({ autoSelectAudio, audioAppExplicitlySet, selectedAudioAppId });
  audioChoiceRef.current = { autoSelectAudio, audioAppExplicitlySet, selectedAudioAppId };

  const { telemetry, setTelemetry, startTelemetryPolling, stopTelemetryPolling, resetStatsPrev } =
    useStreamTelemetry(spectatorCount);

  const activeVideoCodecRef = useRef<VideoCodec>(videoCodec);
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
    setSelectedAudioAppId(autoSelectAudio ? null : 0);
    setAutoDetectFailed(false);

    await createNativeRoom();
  }, [
    setAudioAppExplicitlySet,
    setAutoDetectedApp,
    setSelectedAudioAppId,
    setAutoDetectFailed,
    createNativeRoom,
    autoSelectAudio,
  ]);

  const resetCaptureState = useCallback((): void => {
    captureSessionRef.current += 1;
    lastVideoConfigKeyRef.current = null;
    setCaptureStage('idle');
    stopTelemetryPolling();
    audioAppIdRef.current = null;
    setPreviewFrame(null);
    if (!audioAppExplicitlySet) setSelectedAudioAppId(autoSelectAudio ? null : 0);
    setAudioAppExplicitlySet(false);
    setAutoDetectedApp(null);
    setAutoDetectFailed(false);
  }, [
    stopTelemetryPolling,
    audioAppIdRef,
    audioAppExplicitlySet,
    autoSelectAudio,
    setSelectedAudioAppId,
    setAudioAppExplicitlySet,
    setAutoDetectedApp,
    setAutoDetectFailed,
  ]);

  const stopCaptureAfterRoomClosure = useCallback(async (): Promise<void> => {
    resetCaptureState();
    try {
      await stopAudioCapture();
    } catch (cause) {
      notify('error', 'Audio capture cleanup failed', String(cause));
    }
  }, [resetCaptureState, stopAudioCapture]);

  const handleStopShare = useCallback(async () => {
    if (stopBusyRef.current) return;
    stopBusyRef.current = true;
    captureSessionRef.current += 1;
    setPickerOpen(false);
    setShowStopConfirm(false);
    try {
      const stopped = await desktopApi.stopNativeCapture();
      await stopAudioCapture();
      if (!stopped) throw new Error('Capture could not be stopped cleanly. The room remains open.');
      resetCaptureState();
    } catch (cause) {
      notify('error', 'Screenshare stop failed', String(cause));
    } finally {
      stopBusyRef.current = false;
    }
  }, [resetCaptureState, stopAudioCapture]);

  useEffect(() => {
    const unlistenPromise = desktopApi.onCaptureEnded(() => {
      notify('info', 'Stream ended', 'The captured window was closed, so sharing stopped.');
      void handleStopShare();
    });
    return () => {
      void unlistenPromise.then((unlisten) => unlisten());
    };
  }, [handleStopShare]);

  const resolveAudioTarget = useCallback(async (): Promise<number> => {
    const choice = audioChoiceRef.current;
    if (choice.audioAppExplicitlySet) return choice.selectedAudioAppId ?? 0;
    if (!choice.autoSelectAudio) return 0;

    await loadAudioApps();
    const resolved = await attemptAutoResolve();
    const current = audioChoiceRef.current;
    if (current.audioAppExplicitlySet) return current.selectedAudioAppId ?? 0;
    if (!current.autoSelectAudio) return 0;
    if (resolved) return toAudioTargetId(resolved, platformInfo?.platform === 'linux');
    setCaptureContext(await desktopApi.getCaptureContext());
    setAutoDetectFailed(true);
    notify('info', 'No audio detected', 'Sharing video only. Choose an audio app to include its sound.');

    return 0;
  }, [loadAudioApps, attemptAutoResolve, platformInfo?.platform, setCaptureContext, setAutoDetectFailed]);

  const captureAudioForTarget = useCallback(
    async (requestedTargetAudioId: number | null): Promise<void> => {
      if (requestedTargetAudioId === null) return;
      const choice = audioChoiceRef.current;
      let targetAudioId = requestedTargetAudioId;
      if (choice.audioAppExplicitlySet) targetAudioId = choice.selectedAudioAppId ?? 0;
      else if (!choice.autoSelectAudio) targetAudioId = 0;
      try {
        if (audioAppIdRef.current !== targetAudioId) await startAudioCapture(targetAudioId);
      } catch (err) {
        console.error('Audio capture failed (continuing video-only):', err);
        notify('info', 'Audio unavailable', 'Sharing video only — the selected audio source could not be captured.');
      }
    },
    [audioAppIdRef, startAudioCapture],
  );

  const cleanupFailedShare = useCallback(async (): Promise<void> => {
    const stopped = await desktopApi.stopNativeCapture();
    if (!stopped) notify('error', 'Capture cleanup failed', 'Try Stop Screenshare before sharing again.');
    await stopAudioCapture();
    if (!audioAppExplicitlySet) {
      setSelectedAudioAppId(autoSelectAudio ? null : 0);
      setAutoDetectedApp(null);
    }
  }, [audioAppExplicitlySet, autoSelectAudio, stopAudioCapture, setSelectedAudioAppId, setAutoDetectedApp]);

  const getTelemetryInputs = useCallback(() => {
    const dims = RESOLUTION_DIMENSIONS[resolutionRef.current];
    return {
      width: dims.width,
      height: dims.height,
      targetFrameRate: streamFpsRef.current,
      hasAudio: audioAppIdRef.current != null && audioAppIdRef.current !== 0,
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
      if (captureSessionRef.current !== session) {
        await cleanupFailedShare();
        return;
      }
      await activateLive(session);
    },
    [buildCaptureConfig, activateLive, cleanupFailedShare],
  );

  const runCaptureAction = useCallback(
    async (operation: () => Promise<void>) => {
      if (captureBusyRef.current || stopBusyRef.current || roomActionRef.current || isRoomTransitioning) return;
      captureBusyRef.current = true;
      setIsCaptureBusy(true);
      try {
        await operation();
      } catch (cause) {
        notify('error', 'Screenshare action failed', String(cause));
      } finally {
        captureBusyRef.current = false;
        setIsCaptureBusy(false);
      }
    },
    [isRoomTransitioning],
  );

  const startPreviewCapture = useCallback(async (source?: CaptureSourceSelection): Promise<number> => {
    const session = ++captureSessionRef.current;
    primeAudioContext();
    const started = await desktopApi.startCapturePreview(source);
    if (captureSessionRef.current !== session) {
      if (started && !(await desktopApi.stopVideoCapture()))
        notify('error', 'Capture cleanup failed', 'A cancelled source did not stop cleanly.');
      return session;
    }
    if (!started) throw new Error('No capture source was selected. The room remains open.');
    selectedCaptureSourceRef.current = source ?? null;
    setPreviewFrame(null);
    setCaptureStage('previewing');
    if (trayShareRef.current) {
      trayShareRef.current = false;
      revealMainWindow();
    }

    return session;
  }, []);

  const publishCapture = useCallback(
    async (session: number) => {
      if (captureSessionRef.current !== session) return;
      const published = await desktopApi.goLive(buildCaptureConfig(), selectedCaptureSourceRef.current ?? undefined);
      if (!published) throw new Error('The selected source could not be published. Try Go Live again.');
      if (captureSessionRef.current !== session) {
        await cleanupFailedShare();
        return;
      }
      await activateLive(session);
    },
    [buildCaptureConfig, activateLive, cleanupFailedShare],
  );

  const replaceCaptureSource = useCallback(
    async (source?: CaptureSourceSelection) => {
      const wasLive = captureStage === 'live';
      const previousSession = captureSessionRef.current + 1;
      captureSessionRef.current = previousSession;
      if (captureStage !== 'idle') {
        if (!(await desktopApi.stopVideoCapture())) throw new Error('The current video source could not stop.');
        if (captureSessionRef.current !== previousSession) return;
        stopTelemetryPolling();
        lastVideoConfigKeyRef.current = null;
        setCaptureStage('idle');
        setPreviewFrame(null);
      }
      const session = await startPreviewCapture(source);
      if (wasLive) await publishCapture(session);
    },
    [captureStage, stopTelemetryPolling, startPreviewCapture, publishCapture],
  );

  const handleStartShare = useCallback(async () => {
    if (!roomCode || captureStage !== 'idle' || pickerOpen || !preferencesSnapshot) return;
    if (platformInfo?.platform === 'windows') {
      setPickerOpen(true);
      return;
    }
    await runCaptureAction(async () => {
      await startPreviewCapture();
    });
  }, [roomCode, captureStage, pickerOpen, preferencesSnapshot, platformInfo, runCaptureAction, startPreviewCapture]);

  const ensureTrayRoom = useCallback(async (): Promise<boolean> => {
    if (roomCodeRef.current) return true;
    if (!healthyEndpointRef.current) {
      notify('error', 'API endpoint unavailable', 'Fix the API endpoint in Settings → Stream before sharing.');
      return false;
    }
    return createNativeRoom();
  }, [createNativeRoom]);

  const startShareFromTray = useCallback(async () => {
    if (captureBusyRef.current || roomActionRef.current) return;
    if (!(await ensureTrayRoom())) return;
    if (platformInfo?.platform === 'windows') {
      setPickerOpen(true);
      revealMainWindow();
      return;
    }
    trayShareRef.current = true;
    try {
      await runCaptureAction(async () => {
        await startPreviewCapture();
      });
    } finally {
      if (trayShareRef.current) {
        trayShareRef.current = false;
        revealMainWindow();
      }
    }
  }, [ensureTrayRoom, platformInfo, runCaptureAction, startPreviewCapture]);

  const handleSwitchSource = useCallback(async () => {
    if (!roomCode || captureStage === 'idle' || pickerOpen || captureBusyRef.current || roomActionRef.current) return;
    if (platformInfo?.platform === 'windows') {
      setPickerOpen(true);
      return;
    }
    await runCaptureAction(() => replaceCaptureSource());
  }, [roomCode, captureStage, pickerOpen, platformInfo, runCaptureAction, replaceCaptureSource]);

  const handleSourceSelected = useCallback(
    (selection: CaptureSourceSelection): void => {
      setPickerOpen(false);
      void runCaptureAction(() => replaceCaptureSource(selection));
    },
    [runCaptureAction, replaceCaptureSource],
  );

  const handleGoLive = useCallback(async () => {
    if (captureStage !== 'previewing' || !previewFrame) return;
    await runCaptureAction(() => publishCapture(captureSessionRef.current));
  }, [captureStage, previewFrame, runCaptureAction, publishCapture]);

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

  const handleApiEndpointValidated = useCallback(
    (endpoint: string): void => {
      if (endpoint === apiEndpoint) {
        setHasHealthyApiEndpoint(true);
        if (!roomCode) setPendingApiEndpoint(null);
        return;
      }
      if (endpoint === pendingApiEndpoint && (roomCode || retryableReplacement)) return;
      if (!roomCode && !retryableReplacement) {
        activateApiEndpoint(endpoint);
        setHasHealthyApiEndpoint(true);
        return;
      }

      setEndpointSwitchCandidate(endpoint);
      void desktopApi
        .showMainWindow()
        .catch((cause: unknown) => notify('error', 'Room confirmation unavailable', String(cause)));
    },
    [activateApiEndpoint, apiEndpoint, pendingApiEndpoint, retryableReplacement, roomCode, setPendingApiEndpoint],
  );

  const handleApiEndpointReset = useCallback((): void => {
    setEndpointSwitchCandidate(null);
    setPendingApiEndpoint(null);
  }, [setPendingApiEndpoint]);

  useEffect(() => {
    const subscription = desktopApi.onApiEndpointEdit((edit) => {
      if (edit.kind === 'reset') handleApiEndpointReset();
      else handleApiEndpointValidated(edit.endpoint);
    });
    return () => {
      void subscription.then((stop) => stop());
    };
  }, [handleApiEndpointReset, handleApiEndpointValidated]);

  useEffect(() => {
    if (!settingsHydrated) return;

    let disposed = false;
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 5_000);

    void endpointValidationRevision;
    setEndpointAvailability('checking');
    setHasHealthyApiEndpoint(false);
    void checkApiEndpoint(pendingApiEndpoint ?? apiEndpoint, controller.signal).then((result) => {
      clearTimeout(timeout);
      if (disposed) return;
      setHasHealthyApiEndpoint(result.ok);
      if (result.ok) {
        setEndpointAvailability('healthy');
        handleApiEndpointValidated(result.endpoint);
      } else {
        setEndpointAvailability('error');
      }
    });
    return () => {
      disposed = true;
      clearTimeout(timeout);
      controller.abort();
    };
  }, [apiEndpoint, pendingApiEndpoint, endpointValidationRevision, settingsHydrated, handleApiEndpointValidated]);

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

  const handleCloseRoom = useCallback(async (): Promise<boolean> => {
    if (isRoomTransitioning || roomActionRef.current) return false;
    roomActionRef.current = true;
    setIsRoomTransitioning(true);
    try {
      const result = await closeRoom();
      if (!result.ok) {
        notify('error', 'Room closure failed', result.error ?? 'The room remains open. Try again.');
        return false;
      }
      await stopCaptureAfterRoomClosure();
      if (pendingApiEndpoint) setEndpointValidationRevision((revision) => revision + 1);
      return true;
    } finally {
      roomActionRef.current = false;
      setIsRoomTransitioning(false);
    }
  }, [closeRoom, isRoomTransitioning, pendingApiEndpoint, stopCaptureAfterRoomClosure]);

  const handleToggleMute = useCallback(async () => {
    if (muteBusyRef.current) return;
    muteBusyRef.current = true;
    try {
      setIsMuted(await desktopApi.toggleAudioMute());
    } catch (cause) {
      notify('error', 'Audio mute failed', String(cause));
    } finally {
      muteBusyRef.current = false;
    }
  }, []);

  const handleOpenSettings = useCallback(async () => {
    try {
      await desktopApi.openSettings();
    } catch (cause) {
      notify('error', 'Settings could not open', String(cause));
    }
  }, []);

  const confirmAction = useCallback(async () => {
    const action = confirmation;
    setConfirmation(null);
    if (action === 'close-room') {
      await handleCloseRoom();
      return;
    }
    if (action !== 'quit' && action !== 'quit-anyway') return;
    await handleStopShare();
    if (action === 'quit' && roomCode && !(await handleCloseRoom())) {
      setConfirmation('quit-anyway');
      return;
    }
    await desktopApi.quitApp();
  }, [confirmation, roomCode, handleCloseRoom, handleStopShare]);

  const startShareOrGoLive = useCallback(() => {
    if (captureStage === 'previewing') {
      void handleGoLive();
      return;
    }
    if (captureStage === 'idle') void startShareFromTray();
  }, [captureStage, handleGoLive, startShareFromTray]);

  const dispatchAction = useCallback(
    (action: DesktopAction) => {
      switch (action) {
        case 'start-share':
          startShareOrGoLive();
          return;
        case 'stop-share':
          void handleStopShare();
          return;
        case 'switch-source':
          void handleSwitchSource();
          return;
        case 'toggle-mute':
          void handleToggleMute();
          return;
        case 'close-room':
          if (roomCode) setConfirmation('close-room');
          return;
        case 'quit':
          if (roomCode || captureStage !== 'idle' || captureBusyRef.current) setConfirmation('quit');
          else void desktopApi.quitApp().catch((cause: unknown) => notify('error', 'Quit failed', String(cause)));
      }
    },
    [roomCode, captureStage, handleStopShare, handleSwitchSource, handleToggleMute, startShareOrGoLive],
  );
  const dispatchActionRef = useRef(dispatchAction);
  dispatchActionRef.current = dispatchAction;

  useEffect(() => {
    const subscription = desktopApi.onDesktopAction((action) => dispatchActionRef.current(action));
    void desktopApi
      .getAudioMuted()
      .then(setIsMuted)
      .catch((cause: unknown) => notify('error', 'Mute status unavailable', String(cause)));
    const openSettings = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.repeat || event.isComposing) return;
      if ((event.ctrlKey || event.metaKey) && event.key === ',' && !event.altKey && !event.shiftKey) {
        event.preventDefault();
        void handleOpenSettings();
      }
    };
    const toggleTelemetry = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.repeat || event.isComposing) return;
      if (!document.hasFocus()) return;
      if (
        event.target instanceof Element &&
        event.target.closest('input, textarea, select, [contenteditable]:not([contenteditable="false"])')
      )
        return;
      if (!matchesTelemetryShortcut(event, telemetryShortcutRef.current)) return;
      event.preventDefault();
      setShowTelemetry((visible) => !visible);
    };
    window.addEventListener('keydown', openSettings);
    window.addEventListener('keydown', toggleTelemetry);
    return () => {
      void subscription.then((stop) => stop());
      window.removeEventListener('keydown', openSettings);
      window.removeEventListener('keydown', toggleTelemetry);
    };
  }, [handleOpenSettings]);

  useEffect(() => {
    void desktopApi
      .updateControlState({
        roomOpen: !!roomCode,
        captureStage,
        isMuted,
        isBusy: isCaptureBusy || isRoomTransitioning,
        roomEndpoint,
        endpointControlsDisabled: isRoomTransitioning || retryableReplacement !== null,
        endpointValidationRevision,
      })
      .catch((cause: unknown) => notify('error', 'Tray status could not update', String(cause)));
  }, [
    roomCode,
    captureStage,
    isMuted,
    isCaptureBusy,
    isRoomTransitioning,
    roomEndpoint,
    retryableReplacement,
    endpointValidationRevision,
  ]);

  useEffect(() => {
    if (preferencesError) notify('error', 'Preferences unavailable', preferencesError);
  }, [preferencesError]);

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
    if (endpointAvailability === 'error') return 'Fix the API endpoint in Settings → Stream before creating a room.';
    return null;
  };
  const canStartShare =
    !!roomCode &&
    captureStage === 'idle' &&
    !isRoomTransitioning &&
    !isCaptureBusy &&
    !pickerOpen &&
    preferencesSnapshot !== null;
  const canGoLive = captureStage === 'previewing' && previewFrame !== null && !isRoomTransitioning && !isCaptureBusy;
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
            showTelemetry={showTelemetry}
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
              onCloseRoom={() => setConfirmation('close-room')}
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
              onSwitchSource={handleSwitchSource}
              canSwitchSource={!isCaptureBusy && !isRoomTransitioning && !pickerOpen}
            />

            <AudioAppPicker
              audioApps={audioApps}
              audioAppGroups={audioAppGroups}
              selectedAudioAppId={selectedAudioAppId}
              autoDetectedApp={autoDetectedApp}
              onSelectApp={handleSelectApp}
              onRefresh={loadAudioApps}
              autoSelectAudio={autoSelectAudio}
              isMuted={isMuted}
              onToggleMute={handleToggleMute}
            />
          </div>
        </main>
      </div>
    );
  }

  return (
    <div className="h-screen flex flex-col overflow-hidden">
      <TitleBar
        isLive={captureStage === 'live'}
        isPreviewing={captureStage === 'previewing'}
        onSettings={handleOpenSettings}
      />
      {content}
      <DesktopActionConfirmation
        action={confirmation}
        isBusy={isRoomTransitioning}
        onCancel={() => setConfirmation(null)}
        onConfirm={confirmAction}
      />
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
if (new URLSearchParams(window.location.search).has('settings')) root.render(<DesktopSettings />);
else root.render(<PresenterApp />);
