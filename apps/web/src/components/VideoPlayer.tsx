import React, { useRef, useState, useEffect } from 'react';
import {
  Play,
  Pause,
  Volume2,
  VolumeX,
  Maximize,
  Minimize,
  RefreshCw,
  VideoOff,
  Radio,
} from 'lucide-react';
import { AudioVisualizer } from './AudioVisualizer';
import { Button } from './ui/Button';

interface VideoPlayerProps {
  mediaStream: MediaStream | null;
  isLive: boolean;
  statusText?: string;
  onResync?: () => void;
}

export const VideoPlayer: React.FC<VideoPlayerProps> = ({
  mediaStream,
  isLive,
  statusText,
  onResync,
}) => {
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);

  const [isPlaying, setIsPlaying] = useState(true);
  const [isMuted, setIsMuted] = useState(false);
  const [volume, setVolume] = useState(1);
  const [isFullscreen, setIsFullscreen] = useState(false);
  const [hasVideoTrack, setHasVideoTrack] = useState(false);
  const [showControls, setShowControls] = useState(true);

  const controlsTimeoutRef = useRef<NodeJS.Timeout | null>(null);

  useEffect(() => {
    const video = videoRef.current;
    if (!video) return;

    if (mediaStream) {
      video.srcObject = mediaStream;
      const videoTracks = mediaStream.getVideoTracks();
      setHasVideoTrack(videoTracks.length > 0 && videoTracks[0].enabled);

      video
        .play()
        .then(() => setIsPlaying(true))
        .catch((err) => {
          console.warn('[VideoPlayer] Autoplay prevented, muting to retry:', err);
          video.muted = true;
          setIsMuted(true);
          video.play().then(() => setIsPlaying(true)).catch(console.error);
        });
    } else {
      video.srcObject = null;
      setHasVideoTrack(false);
    }
  }, [mediaStream]);

  const togglePlay = () => {
    const video = videoRef.current;
    if (!video) return;

    if (isPlaying) {
      video.pause();
      setIsPlaying(false);
    } else {
      video.play().then(() => setIsPlaying(true)).catch(console.error);
    }
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
    if (!containerRef.current) return;

    if (!document.fullscreenElement) {
      containerRef.current.requestFullscreen().then(() => setIsFullscreen(true)).catch(console.error);
    } else {
      document.exitFullscreen().then(() => setIsFullscreen(false)).catch(console.error);
    }
  };

  const handleMouseMove = () => {
    setShowControls(true);
    if (controlsTimeoutRef.current) {
      clearTimeout(controlsTimeoutRef.current);
    }
    controlsTimeoutRef.current = setTimeout(() => {
      if (isPlaying && isLive) {
        setShowControls(false);
      }
    }, 3000);
  };

  return (
    <div
      ref={containerRef}
      onMouseMove={handleMouseMove}
      onMouseLeave={() => isPlaying && isLive && setShowControls(false)}
      className="relative w-full aspect-video bg-black rounded-2xl overflow-hidden border border-gray-800 shadow-2xl group select-none flex items-center justify-center"
    >
      {/* HTML5 Video Element */}
      <video
        ref={videoRef}
        playsInline
        className={`w-full h-full object-contain ${hasVideoTrack && isLive ? 'block' : 'hidden'}`}
      />

      {/* Overlay when stream is inactive or loading */}
      {(!isLive || !hasVideoTrack) && (
        <div className="absolute inset-0 flex flex-col items-center justify-center bg-gradient-to-b from-gray-950/90 to-gray-900 p-6 text-center z-10">
          <div className="p-4 bg-gray-800/80 rounded-2xl mb-4 border border-gray-700/50 shadow-inner">
            <Radio className="w-10 h-10 text-indigo-400 animate-pulse" />
          </div>
          <h3 className="text-xl font-semibold text-gray-100 mb-2">
            {statusText || 'Waiting for Presenter Stream'}
          </h3>
          <p className="text-sm text-gray-400 max-w-md mb-6">
            When the presenter starts screensharing from the Desktop App, the video stream will appear here automatically.
          </p>
          {onResync && (
            <Button variant="outline" size="sm" onClick={onResync} className="gap-2">
              <RefreshCw className="w-4 h-4" />
              <span>Re-sync Stream</span>
            </Button>
          )}
        </div>
      )}

      {/* Top Overlay Badge */}
      <div
        className={`absolute top-4 left-4 right-4 flex items-center justify-between z-20 transition-opacity duration-300 ${
          showControls || !isLive ? 'opacity-100' : 'opacity-0 pointer-events-none'
        }`}
      >
        <div className="flex items-center gap-2">
          {isLive ? (
            <span className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full bg-emerald-500/20 border border-emerald-500/30 text-emerald-400 text-xs font-semibold uppercase tracking-wider backdrop-blur-md">
              <span className="w-2 h-2 rounded-full bg-emerald-400 animate-ping" />
              Live Stream
            </span>
          ) : (
            <span className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full bg-amber-500/20 border border-amber-500/30 text-amber-400 text-xs font-semibold uppercase tracking-wider backdrop-blur-md">
              Standby
            </span>
          )}
        </div>

        {/* Audio Visualizer */}
        {isLive && mediaStream && (
          <AudioVisualizer mediaStream={mediaStream} className="backdrop-blur-md" />
        )}
      </div>

      {/* Bottom Controls Bar */}
      {isLive && (
        <div
          className={`absolute bottom-0 inset-x-0 bg-gradient-to-t from-black/90 via-black/50 to-transparent p-4 z-20 transition-opacity duration-300 flex items-center justify-between gap-4 ${
            showControls ? 'opacity-100' : 'opacity-0 pointer-events-none'
          }`}
        >
          <div className="flex items-center gap-3">
            <button
              onClick={togglePlay}
              className="p-2 text-gray-200 hover:text-white bg-gray-800/80 hover:bg-gray-700/80 rounded-xl transition-all border border-gray-700/50"
              title={isPlaying ? 'Pause' : 'Play'}
            >
              {isPlaying ? <Pause className="w-5 h-5" /> : <Play className="w-5 h-5" />}
            </button>

            {/* Volume Control */}
            <div className="flex items-center gap-2 bg-gray-800/80 px-3 py-1.5 rounded-xl border border-gray-700/50">
              <button
                onClick={toggleMute}
                className="text-gray-300 hover:text-white transition-colors"
                title={isMuted ? 'Unmute' : 'Mute'}
              >
                {isMuted || volume === 0 ? (
                  <VolumeX className="w-4 h-4 text-rose-400" />
                ) : (
                  <Volume2 className="w-4 h-4" />
                )}
              </button>
              <input
                type="range"
                min="0"
                max="1"
                step="0.05"
                value={isMuted ? 0 : volume}
                onChange={handleVolumeChange}
                className="w-20 accent-indigo-500 h-1.5 bg-gray-700 rounded-lg cursor-pointer"
              />
            </div>
          </div>

          <div className="flex items-center gap-2">
            {onResync && (
              <button
                onClick={onResync}
                className="p-2 text-gray-300 hover:text-white bg-gray-800/80 hover:bg-gray-700/80 rounded-xl transition-all border border-gray-700/50"
                title="Re-sync WebRTC stream"
              >
                <RefreshCw className="w-4 h-4" />
              </button>
            )}

            <button
              onClick={toggleFullscreen}
              className="p-2 text-gray-300 hover:text-white bg-gray-800/80 hover:bg-gray-700/80 rounded-xl transition-all border border-gray-700/50"
              title="Toggle Fullscreen"
            >
              {isFullscreen ? <Minimize className="w-4 h-4" /> : <Maximize className="w-4 h-4" />}
            </button>
          </div>
        </div>
      )}
    </div>
  );
};
