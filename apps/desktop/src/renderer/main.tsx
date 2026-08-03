import type { VideoCodec } from '@slopcast/shared-types';
import { codecLabel, RESOLUTION_DIMENSIONS } from '@slopcast/shared-types';
import type React from 'react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { Toaster } from '@/components/ui/sonner';
import { AudioAppPicker } from './components/audio/AudioAppPicker';
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
import type { CaptureContext, DesktopSource } from './types';
import { copyText } from './utils/clipboard';
import { codecOptionSuffix } from './utils/codecs';
import './types/electron-api.d.ts';
import './index.css';

// Debug aid: print every live PipeWire audio stream node's full property
// dictionary (the same view pw-dump shows) when a capture starts, so a missed
// auto-resolve can be matched against the real nodes. Fire-and-forget: never
// blocks share start on PipeWire enumeration.
async function logLiveAudioSources(): Promise<void> {
  if (!window.electronAPI?.dumpAudioSources) return;
  try {
    const sources = await window.electronAPI.dumpAudioSources();
    console.log(`[Presenter] live audio sources: ${sources.length}`);
    for (const source of sources) {
      console.log('[Presenter] audio source props:', JSON.stringify(source, null, 2));
    }
  } catch (err) {
    console.warn('[Presenter] live audio source dump failed:', err);
  }
}

// Debug aid: print a fresh capture-context introspection carrying the
// xdg-desktop-portal screencast metadata (portal.screencast.*) for the picked
// window, KWin window PID/caption, and the best-matched audio app. Fire-and-forget.
async function logSelectedApplication(label: string | null, sourceId: string | null): Promise<void> {
  if (!window.electronAPI?.inspectCaptureContext) return;
  try {
    const context = await window.electronAPI.inspectCaptureContext();
    console.log(
      `[Presenter] selected application (trackLabel="${label}", sourceId=${sourceId ?? 'null'}):`,
      JSON.stringify(context, null, 2),
    );
  } catch (err) {
    console.warn('[Presenter] selected application dump failed:', err);
  }
}

export const PresenterApp: React.FC = () => {
  const [isWayland, setIsWayland] = useState<boolean>(false);
  const [desktopSources, setDesktopSources] = useState<DesktopSource[]>([]);
  const [selectedSourceId, setSelectedSourceId] = useState<string>('');
  const [isSharing, setIsSharing] = useState<boolean>(false);
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
  } = useAudioCapture(isSharing);

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
    })();

    return () => {
      disconnectRoom();
    };
  }, [loadDesktopSources, disconnectRoom]);

  // Live encoder settings (fps, bitrate, codec, resolution): restart the
  // native video track with the new config. Debounced so rapid changes
  // coalesce into one track restart.
  useEffect(() => {
    if (!isSharing) {
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
      void window.electronAPI
        ?.updateNativeVideo({
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
  }, [streamFps, bitrateLimit, videoCodec, resolution, isSharing, resetStatsPrev]);

  const handleCreateRoom = useCallback(async () => {
    setAudioAppExplicitlySet(false);
    setAutoDetectedApp(null);
    setSelectedAudioAppId(null);
    setAutoDetectFailed(false);

    await createNativeRoom();
  }, [setAudioAppExplicitlySet, setAutoDetectedApp, setSelectedAudioAppId, setAutoDetectFailed, createNativeRoom]);

  const handleStopShare = useCallback(async () => {
    lastVideoConfigKeyRef.current = null;
    if (window.electronAPI) {
      await window.electronAPI.stopNativeCapture();
      await window.electronAPI.stopAudioCapture();
    }
    stopTelemetryPolling();
    audioAppIdRef.current = null;
    setIsSharing(false);
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
    let ctx: CaptureContext | null = null;
    if (isWayland && window.electronAPI?.getCaptureContext) {
      ctx = await window.electronAPI.getCaptureContext();
      setCaptureContext(ctx);
    }

    const isMonitor = ctx?.sourceType === 'monitor' || (!isWayland && selectedSourceId?.startsWith('screen:'));

    if (!isMonitor) {
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
    isWayland,
    selectedSourceId,
    audioAppExplicitlySet,
    setAutoDetectFailed,
    setCaptureContext,
    setSelectedAudioAppId,
    setAutoDetectedApp,
  ]);

  const resolveAudioTarget = useCallback(async (): Promise<number | null> => {
    let targetAudioId: number | null = selectedAudioAppId;

    if (targetAudioId === null && !audioAppExplicitlySet) {
      await loadAudioApps();

      const opts = isWayland ? {} : { sourceId: selectedSourceId };
      targetAudioId = (await attemptAutoResolve(opts))?.id ?? null;
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
    isWayland,
    selectedSourceId,
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
    if (window.electronAPI) {
      await window.electronAPI.stopNativeCapture();
      await window.electronAPI.stopAudioCapture();
    }
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

  const handleStartShare = useCallback(async () => {
    primeAudioContext();
    try {
      const dims = RESOLUTION_DIMENSIONS[resolutionRef.current];
      const fps = streamFpsRef.current;
      const res = await window.electronAPI?.startNativeCapture(0, {
        fps,
        width: dims.width,
        height: dims.height,
        videoCodec,
        maxBitrate: bitrateLimitRef.current,
      });
      if (!res?.ok) {
        throw new Error(res?.error ?? 'Native capture failed to start');
      }
      lastVideoConfigKeyRef.current = videoConfigKey();

      const targetAudioId = await resolveAudioTarget();
      await captureAudioForTarget(targetAudioId);

      if (!res.videoEnabled) {
        notify(
          'info',
          'Video requires Wayland',
          'Screen video capture is only available on Wayland — sharing audio only.',
        );
      }

      activeVideoCodecRef.current = videoCodec;
      setIsSharing(true);
      setTelemetry({ ...idleTelemetry(), live: true });
      startTelemetryPolling(getTelemetryInputs);
      void logLiveAudioSources();
      void logSelectedApplication(null, null);
    } catch (err: unknown) {
      console.error('Failed to capture screen:', err);
      const message = err instanceof Error ? err.message : 'Unknown capture error';
      notify('error', 'Screenshare failed to start', message);
      await cleanupFailedShare();
    }
  }, [
    resolveAudioTarget,
    captureAudioForTarget,
    bitrateLimitRef,
    streamFpsRef,
    resolutionRef,
    videoCodec,
    videoConfigKey,
    getTelemetryInputs,
    setTelemetry,
    startTelemetryPolling,
    cleanupFailedShare,
  ]);

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

  const handleSelectSource = useCallback(
    (source: DesktopSource) => {
      setSelectedSourceId(source.id);
      void attemptAutoResolve({ sourceId: source.id, nameHint: source.name });
    },
    [attemptAutoResolve],
  );

  const canStartShare = !!roomCode && !isSharing && (isWayland || !!selectedSourceId);
  const startDisabledReason = (): string | null => {
    if (isSharing || canStartShare) return null;
    if (!roomCode) return 'Create a live room to start sharing.';
    return 'Select a window above to start sharing.';
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
          isSharing={isSharing}
          roomCode={roomCode}
          canStartShare={canStartShare}
          copied={copied}
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
            isWayland={isWayland}
            desktopSources={desktopSources}
            selectedSourceId={selectedSourceId}
            onSelectSource={handleSelectSource}
            captureContext={captureContext}
            autoDetectFailed={autoDetectFailed}
            isSharing={isSharing}
            showStopConfirm={showStopConfirm}
            setShowStopConfirm={setShowStopConfirm}
            spectatorCount={spectatorCount}
            canStartShare={canStartShare}
            disabledReason={disabledReason}
            onStartShare={handleStartShare}
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
