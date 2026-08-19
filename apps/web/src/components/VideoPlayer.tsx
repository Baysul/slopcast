import { AlertTriangle, Maximize, Minimize, Pause, Play, Radio, RefreshCw, Volume2, VolumeX } from 'lucide-react';
import type React from 'react';
import { useEffect, useRef, useState } from 'react';
import { AudioVisualizer, unlockAudioContexts } from './AudioVisualizer';
import {
  computeTelemetry,
  createStatsPrev,
  type SpectatorTelemetry,
  SpectatorTelemetryBar,
} from './SpectatorTelemetryBar';
import { Button } from './ui/button';

interface VideoPlayerProps {
  mediaStream: MediaStream | null;
  isLive: boolean;
  statusText?: string;
  onResync?: () => void;
  fullBleed?: boolean;
  getStatsFn?: () => Promise<RTCStatsReport | null>;
  decoderStalled?: boolean;
  stalledCodec?: string | null;
  isFullscreen?: boolean;
  showFullscreenControls?: boolean;
}

const STATS_POLL_MS = 2000;

// Browsers block unmuted autoplay without a gesture: try playing at normal
// volume first, then retry muted. Resolves to whether a user gesture is still
// needed for audio.
async function playWithMuteFallback(video: HTMLVideoElement): Promise<boolean> {
  try {
    await video.play();
    return false;
  } catch {
    video.muted = true;
    try {
      await video.play();
      return true;
    } catch {
      return true;
    }
  }
}

const getOverlayClass = (isFullscreen: boolean, showFullscreenControls: boolean): string => {
  if (isFullscreen) {
    return showFullscreenControls ? 'opacity-100 pointer-events-auto' : 'opacity-0 pointer-events-none';
  }
  return 'opacity-0 group-hover:opacity-100 pointer-events-none group-hover:pointer-events-auto';
};

const applyPlayResult = (
  video: HTMLVideoElement,
  needsGesture: boolean,
  setIsPlaying: (v: boolean) => void,
  setIsMuted: (v: boolean) => void,
  setNeedsUserGesture: (v: boolean) => void,
  warn: boolean,
): void => {
  setIsPlaying(true);
  if (!needsGesture) {
    unlockAudioContexts();
    return;
  }
  video.muted = true;
  setIsMuted(true);
  setNeedsUserGesture(true);
  if (warn) {
    console.warn('[VideoPlayer] Play still blocked after user gesture:', needsGesture);
  }
};

const useSpectatorTelemetry = (
  isLive: boolean,
  getStatsFn: (() => Promise<RTCStatsReport | null>) | undefined,
  mediaStream: MediaStream | null,
): { telemetry: SpectatorTelemetry | null } => {
  const [telemetry, setTelemetry] = useState<SpectatorTelemetry | null>(null);
  const statsPrevRef = useRef<ReturnType<typeof createStatsPrev>>(null);
  const telemetryPollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  useEffect(() => {
    if (telemetryPollRef.current) {
      clearInterval(telemetryPollRef.current);
      telemetryPollRef.current = null;
    }
    statsPrevRef.current = null;
    setTelemetry(null);

    if (!isLive || !getStatsFn) return;

    telemetryPollRef.current = setInterval(async () => {
      const report = await getStatsFn();
      if (!report) return;

      const hasAudio = mediaStream?.getAudioTracks().some((t) => t.enabled) ?? false;
      const t = computeTelemetry(report, statsPrevRef.current, hasAudio);
      statsPrevRef.current = createStatsPrev(report) ?? statsPrevRef.current;
      setTelemetry(t);
    }, STATS_POLL_MS);

    return () => {
      if (telemetryPollRef.current) {
        clearInterval(telemetryPollRef.current);
        telemetryPollRef.current = null;
      }
    };
  }, [isLive, getStatsFn, mediaStream]);

  return { telemetry };
};

interface PlaybackControls {
  videoRef: React.RefObject<HTMLVideoElement | null>;
  containerRef: React.RefObject<HTMLDivElement | null>;
  isPlaying: boolean;
  isMuted: boolean;
  volume: number;
  hasVideoTrack: boolean;
  needsUserGesture: boolean;
  audioTrackCount: number;
  handleUserGesture: () => void;
  togglePlay: () => void;
  toggleMute: () => void;
  handleVolumeChange: (e: React.ChangeEvent<HTMLInputElement>) => void;
  toggleFullscreen: () => void;
}

const fullscreenTarget = (container: HTMLDivElement | null, fullBleed: boolean): HTMLElement => {
  if (!fullBleed) {
    return container || document.documentElement;
  }
  return (container?.closest('.min-h-screen') as HTMLElement) || container || document.documentElement;
};

const usePlaybackControls = (mediaStream: MediaStream | null, fullBleed?: boolean): PlaybackControls => {
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);

  const [isPlaying, setIsPlaying] = useState(false);
  const [isMuted, setIsMuted] = useState(false);
  const [volume, setVolume] = useState(1);
  const [hasVideoTrack, setHasVideoTrack] = useState(false);
  const [needsUserGesture, setNeedsUserGesture] = useState(false);
  const audioTrackCount = mediaStream?.getAudioTracks().length ?? 0;

  useEffect(() => {
    const video = videoRef.current;
    if (!video) return;

    const applyTracks = (): void => {
      const videoTracks = mediaStream?.getVideoTracks() ?? [];
      setHasVideoTrack(videoTracks.at(0)?.enabled === true);
    };

    if (!mediaStream) {
      video.srcObject = null;
      applyTracks();
      setNeedsUserGesture(false);
      setIsPlaying(false);
      setIsMuted(false);
      return;
    }

    video.srcObject = mediaStream;
    applyTracks();
    setNeedsUserGesture(false);
    setIsPlaying(false);
    setIsMuted(false);

    playWithMuteFallback(video).then((needsGesture) => {
      applyPlayResult(video, needsGesture, setIsPlaying, setIsMuted, setNeedsUserGesture, false);
    });
    // RoomPage mutates one stable stream identity in place (track
    // subscribe/unsubscribe), so this effect never re-runs for a track
    // change — listen on the stream itself to keep hasVideoTrack honest.
    mediaStream.addEventListener('addtrack', applyTracks);
    mediaStream.addEventListener('removetrack', applyTracks);
    return () => {
      mediaStream.removeEventListener('addtrack', applyTracks);
      mediaStream.removeEventListener('removetrack', applyTracks);
    };
  }, [mediaStream]);

  const handleUserGesture = () => {
    const video = videoRef.current;
    if (!video) return;

    video.muted = false;
    setIsMuted(false);

    playWithMuteFallback(video)
      .then((needsGesture) => {
        applyPlayResult(video, needsGesture, setIsPlaying, setIsMuted, setNeedsUserGesture, true);
      })
      .catch((err) => {
        console.warn('[VideoPlayer] Play blocked after user gesture:', err);
      });

    setNeedsUserGesture(false);
  };

  const togglePlay = () => {
    const video = videoRef.current;
    if (!video) return;

    if (needsUserGesture) {
      handleUserGesture();
      return;
    }

    if (isPlaying) {
      video.pause();
      setIsPlaying(false);
      return;
    }

    video
      .play()
      .then(() => setIsPlaying(true))
      .catch(console.error);
  };

  const toggleMute = () => {
    const video = videoRef.current;
    if (!video) return;

    video.muted = !isMuted;
    setIsMuted(!isMuted);
  };

  const handleVolumeChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const newVolume = parseFloat(e.target.value);
    setVolume(newVolume);
    const video = videoRef.current;
    if (video) {
      video.volume = newVolume;
      video.muted = newVolume === 0;
      setIsMuted(newVolume === 0);
    }
  };

  const toggleFullscreen = () => {
    if (document.fullscreenElement) {
      document.exitFullscreen().catch(console.error);
      return;
    }
    fullscreenTarget(containerRef.current, !!fullBleed).requestFullscreen().catch(console.error);
  };

  return {
    videoRef,
    containerRef,
    isPlaying,
    isMuted,
    volume,
    hasVideoTrack,
    needsUserGesture,
    audioTrackCount,
    handleUserGesture,
    togglePlay,
    toggleMute,
    handleVolumeChange,
    toggleFullscreen,
  };
};

const WaitingOverlay: React.FC<{ statusText: string | undefined; onResync: (() => void) | undefined }> = ({
  statusText,
  onResync,
}) => (
  <div className="absolute inset-0 flex flex-col items-center justify-center bg-black/90 p-6 text-center z-10">
    <Radio className="w-8 h-8 text-white/20 mb-3" />
    <p className="text-sm text-white/40 max-w-xs">{statusText || 'Waiting for presenter...'}</p>
    {onResync && (
      <Button
        variant="outline"
        size="sm"
        onClick={onResync}
        className="gap-2 mt-6 border-white/10 text-white/50 hover:text-white/80 hover:bg-white/5"
      >
        <RefreshCw className="w-3.5 h-3.5" />
        <span>Reconnect</span>
      </Button>
    )}
  </div>
);

const GestureOverlay: React.FC<{
  isPlaying: boolean;
  audioTrackCount: number;
  onUserGesture: () => void;
}> = ({ isPlaying, audioTrackCount, onUserGesture }) => {
  const GestureIcon = isPlaying ? Volume2 : Play;
  const title = isPlaying ? 'Click to enable audio' : 'Click to watch';
  const subtitle = isPlaying ? 'Video is playing — tap to hear audio' : 'Video and audio will play after click';
  return (
    <div className="absolute inset-0 flex items-center justify-center bg-black/70 z-20">
      <button
        type="button"
        onClick={onUserGesture}
        className="flex flex-col items-center gap-4 px-8 py-6 bg-safelight/20 border border-safelight/40 rounded-2xl
                   text-white hover:bg-safelight/30 transition-all backdrop-blur-md cursor-pointer"
      >
        <GestureIcon className="w-10 h-10 text-safelight" />
        <span className="font-semibold text-base">{title}</span>
        {audioTrackCount > 0 && <span className="text-xs text-white/40">{subtitle}</span>}
      </button>
    </div>
  );
};

const DecoderStallOverlay: React.FC<{
  stalledCodec: string | null | undefined;
  onResync: (() => void) | undefined;
}> = ({ stalledCodec, onResync }) => {
  const detail = stalledCodec
    ? `Receiving ${stalledCodec} packets but no frames are decoding. The stream may use an incompatible codec profile.`
    : 'Receiving video data but frames are not displaying.';
  return (
    <div
      data-decoder-stalled="true"
      className="absolute inset-0 flex flex-col items-center justify-center bg-black/85 z-20 p-6"
    >
      <AlertTriangle className="w-8 h-8 text-safelight mb-3" />
      <p className="text-sm font-medium text-white/90 mb-1">Video decoder issue</p>
      <p className="text-xs text-white/50 mb-5 max-w-xs text-center">{detail}</p>
      {onResync && (
        <Button
          variant="outline"
          size="sm"
          onClick={onResync}
          className="gap-2 border-safelight/20 text-safelight hover:text-safelight-hover hover:bg-safelight/10"
        >
          <RefreshCw className="w-3.5 h-3.5" />
          <span>Reconnect</span>
        </Button>
      )}
    </div>
  );
};

const MediaControls: React.FC<{
  telemetry: SpectatorTelemetry | null;
  isPlaying: boolean;
  isMuted: boolean;
  volume: number;
  isFullscreen: boolean;
  onTogglePlay: () => void;
  onToggleMute: () => void;
  onVolumeChange: (e: React.ChangeEvent<HTMLInputElement>) => void;
  onToggleFullscreen: () => void;
  onResync: (() => void) | undefined;
  overlayClass: string;
}> = ({
  telemetry,
  isPlaying,
  isMuted,
  volume,
  isFullscreen,
  onTogglePlay,
  onToggleMute,
  onVolumeChange,
  onToggleFullscreen,
  onResync,
  overlayClass,
}) => {
  const playLabel = isPlaying ? 'Pause' : 'Play';
  const muteLabel = isMuted ? 'Unmute' : 'Mute';
  const fullscreenLabel = isFullscreen ? 'Exit fullscreen' : 'Enter fullscreen';
  const PlayIcon = isPlaying ? Pause : Play;
  const VolumeIcon = isMuted || volume === 0 ? VolumeX : Volume2;
  const FullscreenIcon = isFullscreen ? Minimize : Maximize;
  const volumeValue = isMuted ? 0 : volume;
  const controlClass =
    'p-2 text-white/70 hover:text-white bg-black/30 hover:bg-black/50 rounded-xl transition-all backdrop-blur-sm cursor-pointer focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-black';
  return (
    <div
      className={`absolute bottom-0 inset-x-0 bg-gradient-to-t from-black/80 to-transparent px-4 pt-12 pb-4 z-20 transition-opacity duration-300 flex items-end justify-between gap-4 ${overlayClass}`}
    >
      {telemetry?.hasVideo && <SpectatorTelemetryBar telemetry={telemetry} />}

      <div className="flex items-center gap-2 shrink-0 ml-auto">
        <button type="button" onClick={onTogglePlay} className={controlClass} title={playLabel} aria-label={playLabel}>
          <PlayIcon className="w-5 h-5" />
        </button>

        <div className="flex items-center gap-2 bg-black/30 px-3 py-1.5 rounded-xl backdrop-blur-sm">
          <button
            type="button"
            onClick={onToggleMute}
            className="text-white/60 hover:text-white transition-colors cursor-pointer focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-black rounded-md"
            title={muteLabel}
            aria-label={muteLabel}
          >
            <VolumeIcon className="w-4 h-4" />
          </button>
          <input
            type="range"
            min="0"
            max="1"
            step="0.05"
            value={volumeValue}
            onChange={onVolumeChange}
            aria-label="Volume"
            className="w-14 accent-safelight h-1 bg-white/20 rounded-full cursor-pointer"
          />
        </div>

        {onResync && (
          <button
            type="button"
            onClick={onResync}
            className={controlClass}
            title="Reconnect stream"
            aria-label="Reconnect stream"
          >
            <RefreshCw className="w-4 h-4" />
          </button>
        )}

        <button
          type="button"
          onClick={onToggleFullscreen}
          className={controlClass}
          title={fullscreenLabel}
          aria-label={fullscreenLabel}
        >
          <FullscreenIcon className="w-4 h-4" />
        </button>
      </div>
    </div>
  );
};

export const VideoPlayer: React.FC<VideoPlayerProps> = ({
  mediaStream,
  isLive,
  statusText,
  onResync,
  fullBleed,
  getStatsFn,
  decoderStalled,
  stalledCodec,
  isFullscreen: propIsFullscreen,
  showFullscreenControls = true,
}) => {
  const isFullscreen = propIsFullscreen ?? false;
  const { telemetry } = useSpectatorTelemetry(isLive, getStatsFn, mediaStream);
  const {
    videoRef,
    containerRef,
    isPlaying,
    isMuted,
    volume,
    hasVideoTrack,
    needsUserGesture,
    audioTrackCount,
    handleUserGesture,
    togglePlay,
    toggleMute,
    handleVolumeChange,
    toggleFullscreen,
  } = usePlaybackControls(mediaStream, fullBleed);

  const overlayControlsClass = getOverlayClass(isFullscreen, showFullscreenControls);

  const showWaiting = !isLive || !hasVideoTrack;
  const showGesture = needsUserGesture && isLive && hasVideoTrack;
  const showStall = decoderStalled && isLive && hasVideoTrack;
  const visualizerStream = isLive && mediaStream ? mediaStream : null;
  const containerClass = fullBleed
    ? 'h-screen max-h-screen'
    : 'aspect-video rounded-2xl overflow-hidden border border-border';
  const videoClass = hasVideoTrack && isLive ? 'block' : 'hidden';
  const cursorClass = isFullscreen && !showFullscreenControls ? 'cursor-none' : '';

  return (
    <div
      ref={containerRef}
      className={`relative w-full bg-black select-none flex items-center justify-center group ${cursorClass} ${containerClass}`}
    >
      {/* biome-ignore lint/a11y/useMediaCaption: streamed screen-share video does not provide captions */}
      <video
        ref={videoRef}
        playsInline
        onDoubleClick={toggleFullscreen}
        className={`w-full h-full object-contain cursor-pointer ${videoClass}`}
      />

      {showWaiting && <WaitingOverlay statusText={statusText} onResync={onResync} />}
      {showGesture && (
        <GestureOverlay isPlaying={isPlaying} audioTrackCount={audioTrackCount} onUserGesture={handleUserGesture} />
      )}
      {showStall && <DecoderStallOverlay stalledCodec={stalledCodec} onResync={onResync} />}

      <div className={`absolute top-4 right-16 z-20 transition-opacity duration-300 ${overlayControlsClass}`}>
        {visualizerStream && <AudioVisualizer mediaStream={visualizerStream} showStatus />}
      </div>

      <MediaControls
        telemetry={telemetry}
        isPlaying={isPlaying}
        isMuted={isMuted}
        volume={volume}
        isFullscreen={isFullscreen}
        onTogglePlay={togglePlay}
        onToggleMute={toggleMute}
        onVolumeChange={handleVolumeChange}
        onToggleFullscreen={toggleFullscreen}
        onResync={onResync}
        overlayClass={overlayControlsClass}
      />
    </div>
  );
};
