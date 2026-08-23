import type { VideoCodec } from '@slopcast/shared-types';
import { codecLabel, RESOLUTION_DIMENSIONS } from '@slopcast/shared-types';
import type React from 'react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { Toaster } from '@/components/ui/sonner';
import { desktopApi } from './api/desktop';
import { AudioAppPicker } from './components/audio/AudioAppPicker';
import { PlatformNotice } from './components/gate/PlatformNotice';
import { TitleBar } from './components/layout/TitleBar';
import { ScreensharePreview } from './components/onboarding/preview/ScreensharePreview';
import { WelcomeBanner } from './components/onboarding/WelcomeBanner';
import { StreamSettingsPanel } from './components/settings/StreamSettingsPanel';
import { SourcePicker } from './components/sources/SourcePicker';
import { idleTelemetry } from './components/telemetry/StreamTelemetryBar';
import { useAudioCapture } from './hooks/useAudioCapture';
import { useNativeRoom } from './hooks/useNativeRoom';
import { useStreamSettings } from './hooks/useStreamSettings';
import { useStreamTelemetry } from './hooks/useStreamTelemetry';
import { notify, primeAudioContext } from './lib/toast';
import type { CaptureSourceSelection, CaptureStage, DesktopCaptureConfig, PlatformInfo, PreviewFrame } from './types';
import { recommendBitrateCap } from './utils/bitrate';
import { copyText } from './utils/clipboard';
import { codecOptionSuffix } from './utils/codecs';
import './index.css';

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
  const selectedCaptureSourceRef = useRef<CaptureSourceSelection | null>(null);

  const {
    apiEndpoint,
    setApiEndpoint,
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
  } = useStreamSettings();

  const {
    roomCode,
    shareUrl,
    spectatorCount,
    isCreatingRoom,
    createRoom: createNativeRoom,
    disconnectRoom,
  } = useNativeRoom({
    apiEndpoint,
    livekitUrl,
  });

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
      disconnectRoom();
    };
  }, [disconnectRoom]);

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

  const handleStopShare = useCallback(async () => {
    captureSessionRef.current += 1;
    lastVideoConfigKeyRef.current = null;
    setCaptureStage('idle');
    stopTelemetryPolling();
    const stopped = await desktopApi.stopNativeCapture();
    if (!stopped) {
      notify('error', 'Screenshare stop failed', 'The room remains open, but capture could not be stopped cleanly.');
    }
    audioAppIdRef.current = null;
    setPreviewFrame(null);
    if (!audioAppExplicitlySet) {
      setSelectedAudioAppId(null);
    }
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

  const canStartShare = !!roomCode && captureStage === 'idle';
  const canGoLive = captureStage === 'previewing' && previewFrame !== null;
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
              copied={copied}
              onCreateRoom={handleCreateRoom}
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
            setApiEndpoint={setApiEndpoint}
          />
        </main>
      </div>
    );
  }

  return (
    <div className="h-screen flex flex-col overflow-hidden">
      <TitleBar isLive={captureStage === 'live'} isPreviewing={captureStage === 'previewing'} />
      {content}
      <Toaster />
    </div>
  );
};

const rootEl = document.getElementById('root');
if (!rootEl) throw new Error('Missing #root element');
const root = createRoot(rootEl);
root.render(<PresenterApp />);
