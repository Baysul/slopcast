import type { VideoCodec } from '@slopcast/shared-types';
import { codecLabel, RESOLUTION_DIMENSIONS } from '@slopcast/shared-types';
import { Track } from 'livekit-client';
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
import { useLiveKitRoom } from './hooks/useLiveKitRoom';
import { useStreamSettings } from './hooks/useStreamSettings';
import { useStreamTelemetry } from './hooks/useStreamTelemetry';
import { notify, primeAudioContext } from './lib/toast';
import type { CaptureContext, DesktopSource } from './types';
import { copyText } from './utils/clipboard';
import { codecOptionSuffix } from './utils/codecs';
import './types/electron-api.d.ts';
import './index.css';

export const PresenterApp: React.FC = () => {
  const [isWayland, setIsWayland] = useState<boolean>(false);
  const [desktopSources, setDesktopSources] = useState<DesktopSource[]>([]);
  const [selectedSourceId, setSelectedSourceId] = useState<string>('');
  const [isSharing, setIsSharing] = useState<boolean>(false);
  const [showStopConfirm, setShowStopConfirm] = useState(false);
  const [copied, setCopied] = useState<'link' | 'code' | null>(null);
  const [previewStream, setPreviewStream] = useState<MediaStream | null>(null);

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
    liveKitRoomRef,
    createRoom: createLiveKitRoom,
  } = useLiveKitRoom({
    apiEndpoint,
    livekitUrl,
    videoCodec,
  });

  const {
    audioApps,
    audioAppGroups,
    audioLevels,
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
    captureAudioTrack,
    replaceAudioTrack,
    attemptAutoResolve,
    handleSelectApp,
  } = useAudioCapture(isSharing, liveKitRoomRef);

  const { telemetry, setTelemetry, startTelemetryPolling, stopTelemetryPolling, resetStatsPrev } = useStreamTelemetry();

  const localStreamRef = useRef<MediaStream | null>(null);
  const activeVideoCodecRef = useRef<VideoCodec>(videoCodec);
  const isCodecSwitchingRef = useRef(false);

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
      liveKitRoomRef.current?.disconnect();
      liveKitRoomRef.current = null;
    };
  }, [loadDesktopSources, liveKitRoomRef]);

  // Live encoder parameter updates
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

        const currentId = audioAppIdRef.current;
        if (currentId != null) {
          const hasAudio = (localStreamRef.current?.getAudioTracks().length ?? 0) > 0;
          if (!hasAudio) {
            console.log('[Presenter] Audio track lost after settings change, re-applying...');
            try {
              await replaceAudioTrack(currentId, room);
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
  }, [streamFps, bitrateLimit, isSharing, replaceAudioTrack, liveKitRoomRef, audioAppIdRef]);

  const replaceVideoCodec = useCallback(
    async (targetCodec: VideoCodec): Promise<void> => {
      const room = liveKitRoomRef.current;
      if (!room) return;

      if (isCodecSwitchingRef.current) return;
      isCodecSwitchingRef.current = true;

      if (room.options?.publishDefaults) {
        room.options.publishDefaults.videoCodec = targetCodec;
      }

      const videoTrack = localStreamRef.current?.getVideoTracks()[0];
      if (!videoTrack || videoTrack.readyState === 'ended') {
        isCodecSwitchingRef.current = false;
        return;
      }

      const videoPub = room.localParticipant.videoTrackPublications.values().next().value;
      const oldTrack = videoPub?.track;

      console.log(`[Presenter] Live video codec switch: ${activeVideoCodecRef.current} -> ${targetCodec}`);

      try {
        if (oldTrack) {
          await room.localParticipant.unpublishTrack(oldTrack, false);
          await new Promise((r) => setTimeout(r, 150));
        }

        await room.localParticipant.publishTrack(videoTrack, {
          source: Track.Source.ScreenShare,
          screenShareEncoding: undefined,
          simulcast: false,
          videoCodec: targetCodec,
        });

        if (targetCodec === 'h264') {
          videoTrack.contentHint = 'motion';
        }

        const newPub = room.localParticipant.videoTrackPublications.values().next().value;
        const sender = (newPub?.track as { sender?: RTCRtpSender } | undefined)?.sender;
        if (sender) {
          try {
            const params = sender.getParameters();
            if (!params.encodings?.length) params.encodings = [{}];
            for (const enc of params.encodings) {
              enc.maxBitrate = bitrateLimitRef.current;
              enc.maxFramerate = streamFpsRef.current;
              enc.scaleResolutionDownBy = 1.0;
              enc.priority = 'high';
              enc.networkPriority = 'high';
              enc.active = true;
            }
            await sender.setParameters(params);
          } catch (err) {
            console.warn('[Presenter] Re-applying encoder parameters after codec switch failed:', err);
          }
        }

        resetStatsPrev();
        activeVideoCodecRef.current = targetCodec;
        notify(
          'info',
          'Video codec updated',
          `Switched video codec to ${codecLabel(targetCodec) ?? targetCodec.toUpperCase()}`,
        );
      } catch (err) {
        console.error('[Presenter] Live video codec switch failed:', err);
        notify('error', 'Codec switch failed', `Could not switch video codec to ${targetCodec.toUpperCase()}`);

        const prevCodec = activeVideoCodecRef.current;
        if (oldTrack && prevCodec && prevCodec !== targetCodec) {
          try {
            await room.localParticipant.publishTrack(videoTrack, {
              source: Track.Source.ScreenShare,
              screenShareEncoding: undefined,
              simulcast: false,
              videoCodec: prevCodec,
            });
            setVideoCodec(prevCodec);
          } catch (revertErr) {
            console.error('[Presenter] Reverting to previous codec also failed:', revertErr);
          }
        }
      } finally {
        isCodecSwitchingRef.current = false;
      }
    },
    [liveKitRoomRef, bitrateLimitRef, streamFpsRef, resetStatsPrev, setVideoCodec],
  );

  // Real-time video codec switching while sharing
  useEffect(() => {
    if (!isSharing) {
      activeVideoCodecRef.current = videoCodec;
      return;
    }

    if (activeVideoCodecRef.current === videoCodec) return;

    void replaceVideoCodec(videoCodec);
  }, [videoCodec, isSharing, replaceVideoCodec]);

  const handleCreateRoom = async () => {
    setAudioAppExplicitlySet(false);
    setAutoDetectedApp(null);
    setSelectedAudioAppId(null);
    setAutoDetectFailed(false);

    await createLiveKitRoom();
  };

  const captureVideoTrack = async (): Promise<MediaStreamTrack> => {
    const dims = RESOLUTION_DIMENSIONS[resolutionRef.current];
    if (isWayland) {
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
          if (!audioAppExplicitlySet) {
            setSelectedAudioAppId(-1);
            setAutoDetectedApp({ id: -1, name: 'Desktop Audio', processId: 0 });
          }
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

      const room = liveKitRoomRef.current;
      if (!room) {
        throw new Error('Not connected to a room');
      }

      let hadExisting = false;
      for (const pub of room.localParticipant.trackPublications.values()) {
        const t = pub.track;
        if (t) {
          await room.localParticipant.unpublishTrack(t);
          hadExisting = true;
        }
      }

      if (hadExisting) {
        await new Promise((r) => setTimeout(r, 100));
      }

      resetStatsPrev();

      await room.localParticipant.publishTrack(videoTrack, {
        source: Track.Source.ScreenShare,
        screenShareEncoding: undefined,
        simulcast: false,
        videoCodec,
      });
      activeVideoCodecRef.current = videoCodec;

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
      setTelemetry({ ...idleTelemetry(), live: true });
      startTelemetryPolling(liveKitRoomRef, localStreamRef);
    } catch (err: unknown) {
      console.error('Failed to capture screen:', err);
      const message = err instanceof Error ? err.message : 'Unknown capture error';
      notify('error', 'Screenshare failed to start', message);
      if (window.electronAPI) {
        await window.electronAPI.stopAudioCapture();
      }
      const failedStream = localStreamRef.current;
      if (failedStream) {
        for (const track of failedStream.getTracks()) {
          track.stop();
        }
        localStreamRef.current = null;
      }
      setPreviewStream(null);
      if (!audioAppExplicitlySet) {
        setSelectedAudioAppId(null);
        setAutoDetectedApp(null);
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

    const room = liveKitRoomRef.current;
    if (room) {
      for (const pub of room.localParticipant.trackPublications.values()) {
        const t = pub.track;
        if (t) {
          await room.localParticipant.unpublishTrack(t);
        }
      }
    }

    stopTelemetryPolling();
    audioAppIdRef.current = null;
    if (window.electronAPI) {
      await window.electronAPI.stopAudioCapture();
    }
    setIsSharing(false);
    if (!audioAppExplicitlySet) {
      setSelectedAudioAppId(null);
    }
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
          previewStream={previewStream}
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
            audioLevels={audioLevels}
            onSelectApp={handleSelectApp}
            onRefresh={loadAudioApps}
          />

          <SourcePicker
            isWayland={isWayland}
            desktopSources={desktopSources}
            selectedSourceId={selectedSourceId}
            onSelectSource={(source) => {
              setSelectedSourceId(source.id);
              void attemptAutoResolve({ sourceId: source.id, nameHint: source.name });
            }}
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
