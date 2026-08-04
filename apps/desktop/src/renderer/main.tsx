import type { VideoCodec } from '@slopcast/shared-types';
import { codecLabel, RESOLUTION_DIMENSIONS } from '@slopcast/shared-types';
import type React from 'react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { Toaster } from '@/components/ui/sonner';
import { desktopApi } from './api/desktop';
import { AudioAppPicker } from './components/audio/AudioAppPicker';
import { WaylandNotice } from './components/gate/WaylandNotice';
import { PresenterHeader } from './components/layout/PresenterHeader';
import { WelcomeBanner } from './components/onboarding/WelcomeBanner';
import { ScreensharePreview } from './components/preview/ScreensharePreview';
import { StreamSettingsPanel } from './components/settings/StreamSettingsPanel';
import { SourcePicker } from './components/sources/SourcePicker';
import { idleTelemetry } from './components/telemetry/StreamTelemetryBar';
import { useAudioCapture } from './hooks/useAudioCapture';
import { useNativeRoom } from './hooks/useNativeRoom';
import { useStreamSettings } from './hooks/useStreamSettings';
import { useStreamTelemetry } from './hooks/useStreamTelemetry';
import { notify, primeAudioContext } from './lib/toast';
import type { CaptureStage, DesktopCaptureConfig, PlatformInfo, PreviewFrame } from './types';
import { copyText } from './utils/clipboard';
import { codecOptionSuffix } from './utils/codecs';
import './index.css';

// E2E-only (MIGRATION §12): the WDIO frontend plugin snapshots the raw Tauri
// core (`window.__wdio_original_core__`) that `browser.tauri.execute` needs
// and forwards console logs. Bundled in every build but only executed when
// the e2e build sets VITE_E2E=1, so production runs never touch it.
if (import.meta.env.VITE_E2E === '1') {
  await import('@wdio/tauri-plugin');
}

// Debug aid: print every live PipeWire audio stream node's full property
// dictionary (the same view pw-dump shows) when a capture starts, so a missed
// auto-resolve can be matched against the real nodes. Fire-and-forget: never
// blocks share start on PipeWire enumeration.
async function logLiveAudioSources(): Promise<void> {
  const sources = await desktopApi.dumpAudioSources();
  console.log(`[Presenter] live audio sources: ${sources.length}`);
  for (const source of sources) {
    console.log('[Presenter] audio source props:', JSON.stringify(source, null, 2));
  }
}

// Debug aid: print a fresh capture-context introspection carrying the
// xdg-desktop-portal screencast metadata (portal.screencast.*) for the picked
// window, KWin window PID/caption, and the best-matched audio app. Fire-and-forget.
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
    streamFpsRef,
    bitrateLimitRef,
    resolutionRef,
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
    attemptAutoResolve,
    handleSelectApp,
  } = useAudioCapture(captureStage === 'live');

  const { telemetry, setTelemetry, startTelemetryPolling, stopTelemetryPolling, resetStatsPrev } = useStreamTelemetry();

  const activeVideoCodecRef = useRef<VideoCodec>(videoCodec);
  // The encoder config actually applied to the native track; share start seeds
  // it so the settings effect never restarts the track on mount.
  const lastVideoConfigKeyRef = useRef<string | null>(null);

  const videoConfigKey = useCallback((): string => {
    const dims = RESOLUTION_DIMENSIONS[resolutionRef.current];
    return JSON.stringify({
      fps: streamFpsRef.current,
      bitrate: bitrateLimitRef.current,
      codec: videoCodec,
      width: dims.width,
      height: dims.height,
    });
  }, [videoCodec, resolutionRef, streamFpsRef, bitrateLimitRef]);

  const buildCaptureConfig = useCallback((): DesktopCaptureConfig => {
    const dims = RESOLUTION_DIMENSIONS[resolutionRef.current];
    return {
      fps: streamFpsRef.current,
      width: dims.width,
      height: dims.height,
      videoCodec,
      maxBitrate: bitrateLimitRef.current,
    };
  }, [resolutionRef, streamFpsRef, bitrateLimitRef, videoCodec]);

  useEffect(() => {
    (async () => {
      const info = await desktopApi.getPlatformInfo();
      setPlatformInfo(info);
    })();

    let disposed = false;
    let unlisten: (() => void) | null = null;
    void desktopApi
      .onPreviewFrame((frame) => {
        if (!disposed) setPreviewFrame(frame);
      })
      .then((un) => {
        if (disposed) {
          un();
          return;
        }
        unlisten = un;
      });

    return () => {
      disposed = true;
      unlisten?.();
      disconnectRoom();
    };
  }, [disconnectRoom]);

  // Live encoder settings (fps, bitrate, codec, resolution): restart the
  // native video track with the new config. Debounced so rapid changes
  // coalesce into one track restart.
  useEffect(() => {
    if (captureStage !== 'live') {
      lastVideoConfigKeyRef.current = null;
      return;
    }

    const dims = RESOLUTION_DIMENSIONS[resolution];
    const key = JSON.stringify({
      fps: streamFps,
      bitrate: bitrateLimit,
      codec: videoCodec,
      width: dims.width,
      height: dims.height,
    });
    if (lastVideoConfigKeyRef.current === key) return;

    const prevCodec = activeVideoCodecRef.current;
    const timeout = setTimeout(() => {
      lastVideoConfigKeyRef.current = key;
      void desktopApi
        .updateNativeVideo({
          fps: streamFps,
          width: dims.width,
          height: dims.height,
          videoCodec,
          maxBitrate: bitrateLimit,
        })
        .then((ok) => {
          if (!ok) return;
          resetStatsPrev();
          activeVideoCodecRef.current = videoCodec;
          console.log(
            `[Presenter] Live encoder update: codec=${videoCodec} fps=${streamFps} bitrate=${(bitrateLimit / 1_000_000).toFixed(0)}Mbps`,
          );
          if (prevCodec !== videoCodec) {
            notify(
              'info',
              'Video codec updated',
              `Switched video codec to ${codecLabel(videoCodec) ?? videoCodec.toUpperCase()}`,
            );
          }
        });
    }, 300);
    return () => clearTimeout(timeout);
  }, [streamFps, bitrateLimit, videoCodec, resolution, captureStage, resetStatsPrev]);

  const handleCreateRoom = useCallback(async () => {
    setAudioAppExplicitlySet(false);
    setAutoDetectedApp(null);
    setSelectedAudioAppId(null);
    setAutoDetectFailed(false);

    await createNativeRoom();
  }, [setAudioAppExplicitlySet, setAutoDetectedApp, setSelectedAudioAppId, setAutoDetectFailed, createNativeRoom]);

  const handleStopShare = useCallback(async () => {
    lastVideoConfigKeyRef.current = null;
    await desktopApi.stopNativeCapture();
    await desktopApi.stopAudioCapture();
    stopTelemetryPolling();
    audioAppIdRef.current = null;
    setCaptureStage('idle');
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

  const resolveSystemAudioFallback = useCallback(async (): Promise<boolean> => {
    setAutoDetectFailed(true);
    const ctx = await desktopApi.getCaptureContext();
    setCaptureContext(ctx);

    if (ctx?.sourceType !== 'monitor') {
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
  }, [audioAppExplicitlySet, setAutoDetectFailed, setCaptureContext, setSelectedAudioAppId, setAutoDetectedApp]);

  const resolveAudioTarget = useCallback(async (): Promise<number | null> => {
    let targetAudioId: number | null = selectedAudioAppId;

    if (targetAudioId === null && !audioAppExplicitlySet) {
      await loadAudioApps();
      targetAudioId = (await attemptAutoResolve())?.id ?? null;
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
        await startAudioCapture(targetAudioId);
      } catch (err) {
        console.error('Audio capture failed (continuing video-only):', err);
        notify('info', 'Audio unavailable', 'Sharing video only — the selected audio source could not be captured.');
      }
    },
    [startAudioCapture],
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

  // Shared tail of both go-live paths: resolve and start audio, mark the
  // applied encoder config, flip the stage to live and start telemetry.
  const activateLive = useCallback(async (): Promise<void> => {
    const targetAudioId = await resolveAudioTarget();
    await captureAudioForTarget(targetAudioId);
    activeVideoCodecRef.current = videoCodec;
    lastVideoConfigKeyRef.current = videoConfigKey();
    setCaptureStage('live');
    setTelemetry({ ...idleTelemetry(), live: true });
    startTelemetryPolling(getTelemetryInputs);
    void logLiveAudioSources();
    void logSelectedApplication(null);
  }, [
    resolveAudioTarget,
    captureAudioForTarget,
    videoCodec,
    videoConfigKey,
    setTelemetry,
    startTelemetryPolling,
    getTelemetryInputs,
  ]);

  // Combined start: publishes the track immediately. Used when the pre-roll
  // backend isn't available, matching the pre-migration behavior.
  const startCombinedShare = useCallback(async (): Promise<void> => {
    const res = await desktopApi.startNativeCapture(buildCaptureConfig());
    if (!res.ok) {
      throw new Error(res.error ?? 'Native capture failed to start');
    }
    await activateLive();
  }, [buildCaptureConfig, activateLive]);

  const handleStartShare = useCallback(async () => {
    primeAudioContext();
    try {
      const previewStarted = await desktopApi.startCapturePreview();
      if (previewStarted) {
        setPreviewFrame(null);
        setCaptureStage('previewing');
        return;
      }
      // Pre-roll unavailable (preview backend not merged yet, MIGRATION §9.2):
      // degrade to the combined start.
      await startCombinedShare();
    } catch (err: unknown) {
      console.error('Failed to capture screen:', err);
      const message = err instanceof Error ? err.message : 'Unknown capture error';
      notify('error', 'Screenshare failed to start', message);
      setCaptureStage('idle');
      await cleanupFailedShare();
    }
  }, [startCombinedShare, cleanupFailedShare]);

  const handleGoLive = useCallback(async () => {
    primeAudioContext();
    try {
      const config = buildCaptureConfig();
      const published = await desktopApi.goLive(config);
      if (!published) {
        // Backend without the preview backend: fall back to the combined start.
        const res = await desktopApi.startNativeCapture(config);
        if (!res.ok) {
          throw new Error(res.error ?? 'Native capture failed to start');
        }
      }
      await activateLive();
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

  if (!platformInfo) return null;
  if (!platformInfo.isWayland) return <WaylandNotice />;

  const canStartShare = !!roomCode && captureStage === 'idle';
  const canGoLive = captureStage === 'previewing' && previewFrame !== null;
  const startDisabledReason = (): string | null => {
    if (captureStage !== 'idle' || canStartShare) return null;
    if (!roomCode) return 'Create a live room to start sharing.';
    return null;
  };
  const disabledReason = startDisabledReason();

  return (
    <div className="min-h-screen flex flex-col">
      <PresenterHeader
        roomCode={roomCode}
        shareUrl={shareUrl}
        spectatorCount={spectatorCount}
        isCreatingRoom={isCreatingRoom}
        copied={copied}
        onCreateRoom={handleCreateRoom}
        onCopyCode={handleCopyCode}
        onCopyLink={handleCopyLink}
      />

      <div className="max-w-5xl mx-auto w-full px-6 pt-6">
        <WelcomeBanner />
      </div>

      <main className="flex-1 max-w-5xl mx-auto w-full px-6 py-8 space-y-8">
        <ScreensharePreview
          captureStage={captureStage}
          roomCode={roomCode}
          copied={copied}
          previewFrame={previewFrame}
          telemetry={telemetry}
          onCopyLink={handleCopyLink}
        />

        <div className="grid grid-cols-1 md:grid-cols-2 gap-8">
          <AudioAppPicker
            audioApps={audioApps}
            audioAppGroups={audioAppGroups}
            selectedAudioAppId={selectedAudioAppId}
            autoDetectedApp={autoDetectedApp}
            onSelectApp={handleSelectApp}
            onRefresh={loadAudioApps}
          />

          <SourcePicker
            captureContext={captureContext}
            autoDetectFailed={autoDetectFailed}
            captureStage={captureStage}
            showStopConfirm={showStopConfirm}
            setShowStopConfirm={setShowStopConfirm}
            spectatorCount={spectatorCount}
            canStartShare={canStartShare}
            canGoLive={canGoLive}
            disabledReason={disabledReason}
            onStartShare={handleStartShare}
            onGoLive={handleGoLive}
            onStopShare={handleStopShare}
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
          apiEndpoint={apiEndpoint}
          setApiEndpoint={setApiEndpoint}
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
