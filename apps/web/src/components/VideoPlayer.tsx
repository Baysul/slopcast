import { Maximize, Minimize, Pause, Play, Radio, RefreshCw, Volume2, VolumeX } from 'lucide-react';
import type React from 'react';
import { useEffect, useRef, useState } from 'react';
import { AudioVisualizer, unlockAudioContexts } from './AudioVisualizer';
import {
  computeTelemetry,
  createStatsPrev,
  type SpectatorTelemetry,
  SpectatorTelemetryBar,
} from './SpectatorTelemetryBar';
import { Button } from './ui/Button';

interface VideoPlayerProps {
  mediaStream: MediaStream | null;
  isLive: boolean;
  statusText?: string;
  onResync?: () => void;
  fullBleed?: boolean;
  getStatsFn?: () => Promise<RTCStatsReport | null>;
}

const STATS_POLL_MS = 2000;

export const VideoPlayer: React.FC<VideoPlayerProps> = ({
  mediaStream,
  isLive,
  statusText,
  onResync,
  fullBleed,
  getStatsFn,
}) => {
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);

  const [isPlaying, setIsPlaying] = useState(true);
  const [isMuted, setIsMuted] = useState(false);
  const [volume, setVolume] = useState(1);
  const [isFullscreen, setIsFullscreen] = useState(false);
  const [hasVideoTrack, setHasVideoTrack] = useState(false);
  const [needsAudioUnlock, setNeedsAudioUnlock] = useState(false);

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

  useEffect(() => {
    const video = videoRef.current;
    if (!video) return;

    if (mediaStream) {
      video.srcObject = mediaStream;
      const videoTracks = mediaStream.getVideoTracks();
      setHasVideoTrack(videoTracks.length > 0 && videoTracks[0].enabled);
      setNeedsAudioUnlock(false);

      const hasAudioTrack = mediaStream.getAudioTracks().length > 0;

      video
        .play()
        .then(() => {
          setIsPlaying(true);
        })
        .catch((err) => {
          console.warn('[VideoPlayer] Autoplay prevented, muting to retry:', err);
          video.muted = true;
          setIsMuted(true);
          video
            .play()
            .then(() => {
              setIsPlaying(true);
              if (hasAudioTrack) {
                setNeedsAudioUnlock(true);
              }
            })
            .catch(console.error);
        });
    } else {
      video.srcObject = null;
      setHasVideoTrack(false);
      setNeedsAudioUnlock(false);
    }
  }, [mediaStream]);

  const handleUnlockAudio = () => {
    const video = videoRef.current;
    if (video) {
      video.muted = false;
      setIsMuted(false);
      unlockAudioContexts();
    }
    setNeedsAudioUnlock(false);
  };

  const togglePlay = () => {
    const video = videoRef.current;
    if (!video) return;

    if (isPlaying) {
      video.pause();
      setIsPlaying(false);
    } else {
      video
        .play()
        .then(() => setIsPlaying(true))
        .catch(console.error);
    }
  };

  const toggleMute = () => {
    const video = videoRef.current;
    if (!video) return;
    if (needsAudioUnlock) {
      handleUnlockAudio();
      return;
    }
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
    if (!containerRef.current) return;

    if (!document.fullscreenElement) {
      containerRef.current
        .requestFullscreen()
        .then(() => setIsFullscreen(true))
        .catch(console.error);
    } else {
      document
        .exitFullscreen()
        .then(() => setIsFullscreen(false))
        .catch(console.error);
    }
  };

  return (
    <div
      ref={containerRef}
      className={`relative w-full bg-black select-none flex items-center justify-center group ${
        fullBleed ? 'h-screen max-h-screen' : 'aspect-video rounded-2xl overflow-hidden border border-border'
      }`}
    >
      {/* biome-ignore lint/a11y/useMediaCaption: streamed screen-share video does not provide captions */}
      <video
        ref={videoRef}
        playsInline
        className={`w-full h-full object-contain ${hasVideoTrack && isLive ? 'block' : 'hidden'}`}
      />

      {(!isLive || !hasVideoTrack) && (
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
      )}

      {needsAudioUnlock && isLive && (
        <div className="absolute inset-0 flex items-center justify-center bg-black/70 z-20">
          <button
            type="button"
            onClick={handleUnlockAudio}
            className="flex items-center gap-3 px-6 py-4 bg-safelight/20 border border-safelight/40 rounded-2xl
                       text-white hover:bg-safelight/30 transition-all backdrop-blur-md cursor-pointer"
          >
            <Volume2 className="w-6 h-6 text-safelight" />
            <span className="font-semibold text-base">Click to enable audio</span>
          </button>
        </div>
      )}

      <div className="absolute top-4 right-16 z-20 opacity-0 group-hover:opacity-100 transition-opacity duration-200 pointer-events-none group-hover:pointer-events-auto">
        {isLive && mediaStream && <AudioVisualizer mediaStream={mediaStream} showStatus />}
      </div>

      {isLive && (
        <div className="absolute bottom-0 inset-x-0 bg-gradient-to-t from-black/80 to-transparent px-4 pt-12 pb-4 z-20 opacity-0 group-hover:opacity-100 transition-opacity duration-200 pointer-events-none group-hover:pointer-events-auto flex items-end justify-between gap-4">
          {telemetry?.hasVideo && <SpectatorTelemetryBar telemetry={telemetry} />}

          <div className="flex items-center gap-2 shrink-0 ml-auto">
            <button
              type="button"
              onClick={togglePlay}
              className="p-2 text-white/70 hover:text-white bg-black/30 hover:bg-black/50 rounded-xl transition-all backdrop-blur-sm"
              title={isPlaying ? 'Pause' : 'Play'}
            >
              {isPlaying ? <Pause className="w-5 h-5" /> : <Play className="w-5 h-5" />}
            </button>

            <div className="flex items-center gap-2 bg-black/30 px-3 py-1.5 rounded-xl backdrop-blur-sm">
              <button
                type="button"
                onClick={toggleMute}
                className="text-white/60 hover:text-white transition-colors"
                title={isMuted || needsAudioUnlock ? 'Unmute' : 'Mute'}
              >
                {isMuted || volume === 0 ? <VolumeX className="w-4 h-4" /> : <Volume2 className="w-4 h-4" />}
              </button>
              <input
                type="range"
                min="0"
                max="1"
                step="0.05"
                value={isMuted ? 0 : volume}
                onChange={handleVolumeChange}
                className="w-14 accent-safelight h-1 bg-white/20 rounded-full cursor-pointer"
              />
            </div>

            {onResync && (
              <button
                type="button"
                onClick={onResync}
                className="p-2 text-white/70 hover:text-white bg-black/30 hover:bg-black/50 rounded-xl transition-all backdrop-blur-sm"
                title="Reconnect stream"
              >
                <RefreshCw className="w-4 h-4" />
              </button>
            )}

            {!fullBleed && (
              <button
                type="button"
                onClick={toggleFullscreen}
                className="p-2 text-white/70 hover:text-white bg-black/30 hover:bg-black/50 rounded-xl transition-all backdrop-blur-sm"
                title="Toggle fullscreen"
              >
                {isFullscreen ? <Minimize className="w-4 h-4" /> : <Maximize className="w-4 h-4" />}
              </button>
            )}
          </div>
        </div>
      )}
    </div>
  );
};
