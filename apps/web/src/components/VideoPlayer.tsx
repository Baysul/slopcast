import {
  AlertTriangle,
  Maximize,
  Minimize,
  Pause,
  Play,
  Radio,
  RefreshCw,
  Settings2,
  Volume2,
  VolumeX,
} from 'lucide-react';
import type React from 'react';
import { useEffect, useRef, useState } from 'react';
import { AudioVisualizer, unlockAudioContexts } from './AudioVisualizer';
import { type UseViewerReplayResult, useViewerReplay } from './replay/use-viewer-replay';
import {
  REPLAY_LIVE_TOLERANCE_SECONDS,
  REPLAY_MAX_SECONDS,
  REPLAY_STEP_SECONDS,
  type ViewerReplaySnapshot,
} from './replay/viewer-replay';
import {
  computeTelemetry,
  createStatsPrev,
  type SpectatorTelemetry,
  SpectatorTelemetryBar,
} from './SpectatorTelemetryBar';
import { Button } from './ui/button';
import { Popover, PopoverContent, PopoverTrigger } from './ui/popover';
import { Slider } from './ui/slider';
import { Switch } from './ui/switch';

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
const MAX_PRESENTATION_TRACE_FRAMES = 20_000;
const TELEMETRY_VISIBILITY_STORAGE_KEY = 'slopcast:spectator-telemetry-visible';

const readTelemetryVisibility = (): boolean => {
  try {
    return window.localStorage.getItem(TELEMETRY_VISIBILITY_STORAGE_KEY) === 'true';
  } catch (error) {
    console.warn('[VideoPlayer] Could not read the telemetry preference:', error);
    return false;
  }
};

const saveTelemetryVisibility = (isVisible: boolean): void => {
  try {
    window.localStorage.setItem(TELEMETRY_VISIBILITY_STORAGE_KEY, String(isVisible));
  } catch (error) {
    console.warn('[VideoPlayer] Could not save the telemetry preference:', error);
  }
};

interface PresentationFrame {
  callbackTime: number;
  callbackGap: number | null;
  mediaTime: number;
  mediaTimeGap: number | null;
  presentationTime: number;
  presentationGap: number | null;
  expectedDisplayTime: number;
  expectedDisplayLead: number;
  presentedFrames: number;
  processingDuration: number | null;
}

interface PresentationSummary {
  frameInterval: number;
  medianGap: number | null;
  p95Gap: number | null;
  p99Gap: number | null;
  maximumGap: number | null;
  aboveOnePointFiveIntervals: number;
  aboveTwoIntervals: number;
  presentedFrames: number;
}

interface PlaybackDiagnostics {
  frames: PresentationFrame[];
  summary: (fps?: number) => PresentationSummary;
}

declare global {
  interface Window {
    __slopcastPlaybackDiagnostics?: PlaybackDiagnostics;
  }
}

const diagnosticEnabled = (): boolean => new URLSearchParams(window.location.search).has('diagnostics');

const percentile = (values: number[], percentileValue: number): number | null => {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.floor((sorted.length - 1) * percentileValue))] ?? null;
};

const summarizePresentation = (frames: PresentationFrame[], fps = 60): PresentationSummary => {
  const frameInterval = 1000 / fps;
  const gaps = frames.flatMap((frame) => (frame.presentationGap == null ? [] : [frame.presentationGap]));
  const maximumGap = gaps.length === 0 ? null : Math.max(...gaps);

  return {
    frameInterval,
    medianGap: percentile(gaps, 0.5),
    p95Gap: percentile(gaps, 0.95),
    p99Gap: percentile(gaps, 0.99),
    maximumGap,
    aboveOnePointFiveIntervals: gaps.filter((gap) => gap > frameInterval * 1.5).length,
    aboveTwoIntervals: gaps.filter((gap) => gap > frameInterval * 2).length,
    presentedFrames: frames.length,
  };
};

const usePlaybackDiagnostics = (videoRef: React.RefObject<HTMLVideoElement | null>): void => {
  useEffect(() => {
    const video = videoRef.current;
    if (!video || !diagnosticEnabled()) return;

    const frames: PresentationFrame[] = [];
    let previous: PresentationFrame | null = null;
    let callbackId = 0;
    const diagnostics: PlaybackDiagnostics = {
      frames,
      summary: (fps = 60) => summarizePresentation(frames, fps),
    };
    window.__slopcastPlaybackDiagnostics = diagnostics;

    const recordFrame = (now: DOMHighResTimeStamp, metadata: VideoFrameCallbackMetadata): void => {
      const frame: PresentationFrame = {
        callbackTime: now,
        callbackGap: previous == null ? null : now - previous.callbackTime,
        mediaTime: metadata.mediaTime,
        mediaTimeGap: previous == null ? null : metadata.mediaTime - previous.mediaTime,
        presentationTime: metadata.presentationTime,
        presentationGap: previous == null ? null : metadata.presentationTime - previous.presentationTime,
        expectedDisplayTime: metadata.expectedDisplayTime,
        expectedDisplayLead: metadata.expectedDisplayTime - now,
        presentedFrames: metadata.presentedFrames,
        processingDuration: metadata.processingDuration ?? null,
      };
      frames.push(frame);
      if (frames.length > MAX_PRESENTATION_TRACE_FRAMES) frames.shift();
      previous = frame;
      callbackId = video.requestVideoFrameCallback(recordFrame);
    };

    callbackId = video.requestVideoFrameCallback(recordFrame);
    return () => {
      video.cancelVideoFrameCallback(callbackId);
    };
  }, [videoRef]);
};

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
  return 'opacity-0 group-hover:opacity-100 pointer-events-none group-hover:pointer-events-auto [@media(hover:none)]:opacity-100 [@media(hover:none)]:pointer-events-auto';
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

interface SpectatorTelemetryState {
  telemetry: SpectatorTelemetry | null;
}

const useSpectatorTelemetry = (
  isVisible: boolean,
  isLive: boolean,
  getStatsFn: (() => Promise<RTCStatsReport | null>) | undefined,
  mediaStream: MediaStream | null,
): SpectatorTelemetryState => {
  const [telemetry, setTelemetry] = useState<SpectatorTelemetry | null>(null);

  useEffect(() => {
    setTelemetry(null);
    if (!isVisible || !isLive || !getStatsFn) return;

    let isCancelled = false;
    let statsPrev: ReturnType<typeof createStatsPrev> = null;
    const pollTelemetry = async (): Promise<void> => {
      const report = await getStatsFn();
      if (!report || isCancelled) return;

      const hasAudio = mediaStream?.getAudioTracks().some((track) => track.enabled) ?? false;
      const nextTelemetry = computeTelemetry(report, statsPrev, hasAudio);
      statsPrev = createStatsPrev(report) ?? statsPrev;

      setTelemetry(nextTelemetry);
    };
    const interval = setInterval(() => {
      void pollTelemetry();
    }, STATS_POLL_MS);

    void pollTelemetry();
    return () => {
      isCancelled = true;
      clearInterval(interval);
    };
  }, [isVisible, isLive, getStatsFn, mediaStream]);

  return { telemetry };
};

interface PlaybackControls {
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
  return container?.closest<HTMLElement>('.min-h-screen') || container || document.documentElement;
};

const usePlaybackControls = (
  mediaStream: MediaStream | null,
  fullBleed: boolean | undefined,
  liveVideoRef: React.RefObject<HTMLVideoElement | null>,
  replayVideoRef: React.RefObject<HTMLVideoElement | null>,
  replay: UseViewerReplayResult,
): PlaybackControls => {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [isPlaying, setIsPlaying] = useState(false);
  const [isMuted, setIsMuted] = useState(false);
  const [volume, setVolume] = useState(1);
  const [hasVideoTrack, setHasVideoTrack] = useState(false);
  const [needsUserGesture, setNeedsUserGesture] = useState(false);
  const audioTrackCount = mediaStream?.getAudioTracks().length ?? 0;

  useEffect(() => {
    const video = liveVideoRef.current;
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
      return;
    }

    video.srcObject = mediaStream;
    applyTracks();
    setNeedsUserGesture(false);

    playWithMuteFallback(video).then((needsGesture) => {
      applyPlayResult(video, needsGesture, setIsPlaying, setIsMuted, setNeedsUserGesture, false);
    });
    mediaStream.addEventListener('addtrack', applyTracks);
    mediaStream.addEventListener('removetrack', applyTracks);
    return () => {
      mediaStream.removeEventListener('addtrack', applyTracks);
      mediaStream.removeEventListener('removetrack', applyTracks);
    };
  }, [liveVideoRef, mediaStream]);

  useEffect(() => {
    const liveVideo = liveVideoRef.current;
    const replayVideo = replayVideoRef.current;
    const activeVideo = replay.snapshot.mode === 'replay' ? replayVideo : liveVideo;
    if (!activeVideo) return;

    if (liveVideo) {
      liveVideo.volume = volume;
      liveVideo.muted = replay.snapshot.mode === 'replay' || isMuted;
    }
    if (replayVideo) {
      replayVideo.volume = volume;
      replayVideo.muted = replay.snapshot.mode === 'live' || isMuted;
    }

    const syncPlaying = (): void => setIsPlaying(!activeVideo.paused);
    syncPlaying();
    activeVideo.addEventListener('play', syncPlaying);
    activeVideo.addEventListener('pause', syncPlaying);
    return () => {
      activeVideo.removeEventListener('play', syncPlaying);
      activeVideo.removeEventListener('pause', syncPlaying);
    };
  }, [isMuted, liveVideoRef, replay.snapshot.mode, replayVideoRef, volume]);

  const handleUserGesture = () => {
    const video = liveVideoRef.current;
    if (!video) return;

    video.muted = false;
    setIsMuted(false);

    playWithMuteFallback(video)
      .then((needsGesture) => {
        applyPlayResult(video, needsGesture, setIsPlaying, setIsMuted, setNeedsUserGesture, true);
      })
      .catch((error) => {
        console.warn('[VideoPlayer] Play blocked after user gesture:', error);
      });

    setNeedsUserGesture(false);
  };

  const toggleReplayPlayback = (): void => {
    const replayVideo = replayVideoRef.current;
    if (!replayVideo) return;

    if (replayVideo.paused) {
      replay.playReplay();
      return;
    }
    replay.pauseReplay();
  };

  const toggleLivePlayback = (): void => {
    const liveVideo = liveVideoRef.current;
    if (!liveVideo) return;

    if (!liveVideo.paused) {
      const range = replay.snapshot.range;
      if (range) {
        replay.seek(Math.max(range.start, range.end - 0.05), false);
        return;
      }
      liveVideo.pause();
      return;
    }

    liveVideo.play().catch((error) => {
      console.warn('[VideoPlayer] Live playback could not resume:', error);
    });
  };

  const togglePlay = () => {
    if (needsUserGesture && replay.snapshot.mode === 'live') {
      handleUserGesture();
      return;
    }
    if (replay.snapshot.mode === 'replay') {
      toggleReplayPlayback();
      return;
    }
    toggleLivePlayback();
  };

  const toggleMute = () => {
    const nextMuted = !isMuted;
    const liveVideo = liveVideoRef.current;
    const replayVideo = replayVideoRef.current;

    setIsMuted(nextMuted);
    if (liveVideo) liveVideo.muted = replay.snapshot.mode === 'replay' || nextMuted;
    if (replayVideo) replayVideo.muted = replay.snapshot.mode === 'live' || nextMuted;
  };

  const handleVolumeChange = (event: React.ChangeEvent<HTMLInputElement>) => {
    const nextVolume = Number(event.target.value);
    const nextMuted = nextVolume === 0;
    const liveVideo = liveVideoRef.current;
    const replayVideo = replayVideoRef.current;

    setVolume(nextVolume);
    setIsMuted(nextMuted);
    if (liveVideo) {
      liveVideo.volume = nextVolume;
      liveVideo.muted = replay.snapshot.mode === 'replay' || nextMuted;
    }
    if (replayVideo) {
      replayVideo.volume = nextVolume;
      replayVideo.muted = replay.snapshot.mode === 'live' || nextMuted;
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

const formatReplayTime = (seconds: number): string => {
  const rounded = Math.max(0, Math.round(seconds));
  const minutes = Math.floor(rounded / 60);
  const remainingSeconds = rounded % 60;

  return `${minutes}:${remainingSeconds.toString().padStart(2, '0')}`;
};

const replayPositionLabel = (snapshot: ViewerReplaySnapshot, mediaTime: number): string => {
  const range = snapshot.range;
  if (!range) return 'LIVE';

  const behindSeconds = Math.max(0, range.end - mediaTime);
  if (!snapshot.isShareEnded && behindSeconds <= REPLAY_LIVE_TOLERANCE_SECONDS) return 'LIVE';

  return `-${formatReplayTime(behindSeconds)}`;
};

const ReplayTimeline: React.FC<{
  replay: UseViewerReplayResult;
  isPlaying: boolean;
}> = ({ replay, isPlaying }) => {
  const { snapshot } = replay;
  const range = snapshot.range;
  const timelineRef = useRef<HTMLDivElement | null>(null);
  const [dragPosition, setDragPosition] = useState<number | null>(null);
  const [hoverPosition, setHoverPosition] = useState<number | null>(null);
  if (!range) return null;

  const position = Math.min(range.end, Math.max(range.start, dragPosition ?? snapshot.position));
  const previewPosition = hoverPosition ?? dragPosition;
  const behindSeconds = Math.max(0, range.end - position);
  const isBehindLive = snapshot.mode === 'replay' && behindSeconds > REPLAY_LIVE_TOLERANCE_SECONDS;
  let previewPercent = 0;
  if (previewPosition != null && range.end !== range.start) {
    previewPercent = ((previewPosition - range.start) / (range.end - range.start)) * 100;
  }
  let positionStatus = replayPositionLabel(snapshot, position);
  if (snapshot.isShareEnded) {
    positionStatus = 'Stream ended';
  }
  const shouldShowPositionStatus = positionStatus !== 'LIVE';

  const seekByKeyboard = (event: React.KeyboardEvent<HTMLDivElement>): void => {
    let target: number | null = null;
    if (event.key === 'ArrowLeft') target = position - 5;
    if (event.key === 'ArrowRight') target = position + 5;
    if (event.key === 'Home') target = range.start;
    if (event.key === 'End') target = range.end;
    if (target == null) return;

    event.preventDefault();
    const clamped = Math.min(range.end, Math.max(range.start, target));
    if (event.key === 'End' && !snapshot.isShareEnded) {
      replay.goLive();
      return;
    }
    replay.seek(clamped, isPlaying);
  };

  const updatePointerPreview = (event: React.PointerEvent<HTMLDivElement>): void => {
    const rect = timelineRef.current?.getBoundingClientRect();
    if (!rect || rect.width === 0) return;

    const ratio = Math.min(1, Math.max(0, (event.clientX - rect.left) / rect.width));
    const mediaTime = range.start + ratio * (range.end - range.start);
    setHoverPosition(mediaTime);
    replay.preview(mediaTime);
  };

  const clearPointerPreview = (): void => {
    setHoverPosition(null);
    replay.preview(null);
  };

  return (
    <div className="w-full space-y-2" data-replay-timeline="true">
      <div
        ref={timelineRef}
        className="relative pt-16"
        onPointerMove={updatePointerPreview}
        onPointerLeave={clearPointerPreview}
      >
        {previewPosition != null && (
          <div
            className="absolute top-0 -translate-x-1/2 pointer-events-none"
            style={{ left: `${Math.min(92, Math.max(8, previewPercent))}%` }}
          >
            <div className="overflow-hidden rounded-md border border-white/15 bg-black/90 shadow-lg">
              {snapshot.preview && (
                <img
                  src={snapshot.preview.url}
                  alt=""
                  className="block aspect-video w-40 object-contain bg-black"
                  aria-hidden="true"
                />
              )}
              <div className="px-2 py-1 text-center text-xs font-mono tabular-nums text-foreground">
                {replayPositionLabel(snapshot, previewPosition)}
              </div>
            </div>
          </div>
        )}

        <Slider
          min={range.start}
          max={range.end}
          step={0.1}
          value={[position]}
          aria-label="Replay position"
          aria-valuetext={replayPositionLabel(snapshot, position)}
          onKeyDown={seekByKeyboard}
          onValueChange={(values) => {
            const next = values[0];
            if (next == null) return;
            setDragPosition(next);
            replay.preview(next);
          }}
          onValueCommit={(values) => {
            const next = values[0];
            setDragPosition(null);
            if (next != null) replay.seek(next, isPlaying);
          }}
          className="h-11 [&_[data-slot=slider-track]]:h-1 [&_[data-slot=slider-range]]:bg-white/70 [&_[data-slot=slider-thumb]]:size-4 [&_[data-slot=slider-thumb]]:border-white/80 [&_[data-slot=slider-thumb]]:bg-white"
        />
      </div>

      <div className="grid grid-cols-[1fr_auto_1fr] items-center gap-3 text-xs font-mono tabular-nums text-white/55">
        <span>-{formatReplayTime(range.end - range.start)}</span>
        {shouldShowPositionStatus ? (
          <span role="status" aria-live="polite" className="min-w-0 truncate text-center text-white/75">
            {positionStatus}
          </span>
        ) : (
          <span aria-hidden="true" />
        )}
        {isBehindLive && !snapshot.isShareEnded ? (
          <button
            type="button"
            onClick={replay.goLive}
            className="justify-self-end rounded-sm font-sans font-semibold text-safelight hover:text-safelight-hover focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70"
          >
            Go Live
          </button>
        ) : (
          <span aria-hidden="true" />
        )}
      </div>
    </div>
  );
};

const PlayerSettings: React.FC<{
  replay: UseViewerReplayResult;
  controlClass: string;
  isTelemetryVisible: boolean;
  onTelemetryVisibilityChange: (isVisible: boolean) => void;
}> = ({ replay, controlClass, isTelemetryVisible, onTelemetryVisibilityChange }) => {
  const { snapshot } = replay;
  const isReplayAvailable = snapshot.availability === 'available';
  const settingLabel = snapshot.windowSeconds === 0 ? 'Off' : formatReplayTime(snapshot.windowSeconds);
  const retainedSeconds = snapshot.range ? snapshot.range.end - snapshot.range.start : 0;

  return (
    <Popover>
      <PopoverTrigger asChild>
        <button type="button" className={controlClass} title="Player settings" aria-label="Player settings">
          <Settings2 className="w-4 h-4" />
        </button>
      </PopoverTrigger>
      <PopoverContent
        aria-label="Player settings"
        side="top"
        align="end"
        sideOffset={12}
        className="w-72 border-white/10 bg-black/90 text-foreground backdrop-blur-md shadow-xl"
      >
        <div className="space-y-4">
          <div className="flex items-center justify-between gap-4">
            <div className="min-w-0">
              <label
                htmlFor="spectator-telemetry"
                className="text-xs font-semibold uppercase tracking-wider text-muted-foreground"
              >
                Telemetry
              </label>
              <p className="mt-1 text-xs leading-relaxed text-caption-text">Show incoming stream statistics.</p>
            </div>
            <Switch
              id="spectator-telemetry"
              data-testid="spectator-telemetry-toggle"
              checked={isTelemetryVisible}
              onCheckedChange={onTelemetryVisibilityChange}
              aria-label="Show telemetry"
            />
          </div>

          {isReplayAvailable && (
            <div className="space-y-4 border-t border-white/10 pt-4">
              <div className="flex items-start justify-between gap-4">
                <div>
                  <p className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">Replay window</p>
                  <p className="mt-1 text-xs leading-relaxed text-caption-text">Temporary media stays in this tab.</p>
                </div>
                <span className="text-sm font-mono font-semibold tabular-nums text-foreground">{settingLabel}</span>
              </div>

              <Slider
                min={0}
                max={REPLAY_MAX_SECONDS}
                step={REPLAY_STEP_SECONDS}
                value={[snapshot.windowSeconds]}
                aria-label="Replay buffer duration"
                aria-valuetext={settingLabel}
                onValueChange={(values) => {
                  const next = values[0];
                  if (next != null) replay.setWindowSeconds(next);
                }}
              />

              <div className="flex justify-between text-xs text-caption-text">
                <span>Off</span>
                <span>5 min</span>
              </div>

              {snapshot.limitationReason && (
                <p role="status" className="text-xs leading-relaxed text-safelight">
                  {snapshot.limitationReason}
                </p>
              )}
              {!snapshot.limitationReason && retainedSeconds > 0 && snapshot.windowSeconds > 0 && (
                <p className="text-xs leading-relaxed text-caption-text">
                  {formatReplayTime(retainedSeconds)} currently available
                </p>
              )}
            </div>
          )}
        </div>
      </PopoverContent>
    </Popover>
  );
};

const MediaControls: React.FC<{
  telemetry: SpectatorTelemetry | null;
  isTelemetryVisible: boolean;
  onTelemetryVisibilityChange: (isVisible: boolean) => void;
  replay: UseViewerReplayResult;
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
  isTelemetryVisible,
  onTelemetryVisibilityChange,
  replay,
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
    'p-2 text-white/70 hover:text-white bg-black/30 hover:bg-black/50 rounded-xl transition-colors backdrop-blur-sm cursor-pointer focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-black';
  const showTimeline =
    replay.snapshot.availability === 'available' && replay.snapshot.windowSeconds > 0 && replay.snapshot.range != null;

  return (
    <div
      className={`absolute bottom-0 inset-x-0 bg-gradient-to-t from-black/90 via-black/55 to-transparent px-4 pt-20 pb-4 z-20 transition-opacity duration-300 ${overlayClass}`}
    >
      <div className="mx-auto flex w-full max-w-[1600px] flex-col gap-3">
        {showTimeline && <ReplayTimeline replay={replay} isPlaying={isPlaying} />}

        <div className="flex flex-col items-stretch gap-3 sm:flex-row sm:items-end sm:justify-between sm:gap-4">
          {isTelemetryVisible && telemetry?.hasVideo && (
            <div className="min-w-0">
              <p className="mb-1.5 text-xs font-semibold uppercase tracking-wider text-muted-foreground">Incoming</p>
              <SpectatorTelemetryBar telemetry={telemetry} />
            </div>
          )}

          <div className="flex w-full items-center justify-end gap-2 sm:ml-auto sm:w-auto sm:shrink-0">
            <button
              type="button"
              onClick={onTogglePlay}
              className={controlClass}
              title={playLabel}
              aria-label={playLabel}
            >
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

            <PlayerSettings
              replay={replay}
              controlClass={controlClass}
              isTelemetryVisible={isTelemetryVisible}
              onTelemetryVisibilityChange={onTelemetryVisibilityChange}
            />

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
      </div>
    </div>
  );
};

const VideoLayers: React.FC<{
  liveVideoRef: React.RefObject<HTMLVideoElement | null>;
  replayVideoRef: React.RefObject<HTMLVideoElement | null>;
  captureVideoRef: React.RefObject<HTMLVideoElement | null>;
  liveVideoClass: string;
  replayVideoClass: string;
  onToggleFullscreen: () => void;
}> = ({ liveVideoRef, replayVideoRef, captureVideoRef, liveVideoClass, replayVideoClass, onToggleFullscreen }) => (
  <>
    {/* biome-ignore lint/a11y/useMediaCaption: streamed screen-share video does not provide captions */}
    <video
      ref={liveVideoRef}
      playsInline
      onDoubleClick={onToggleFullscreen}
      className={`absolute inset-0 w-full h-full object-contain cursor-pointer ${liveVideoClass}`}
    />
    {/* biome-ignore lint/a11y/useMediaCaption: locally buffered screen-share video does not provide captions */}
    <video
      ref={replayVideoRef}
      playsInline
      onDoubleClick={onToggleFullscreen}
      className={`absolute inset-0 w-full h-full object-contain cursor-pointer ${replayVideoClass}`}
    />
    <video
      ref={captureVideoRef}
      playsInline
      muted
      aria-hidden="true"
      tabIndex={-1}
      className="absolute size-px opacity-0 pointer-events-none"
    />
  </>
);

const PlayerOverlays: React.FC<{
  showWaiting: boolean;
  showGesture: boolean;
  showStall: boolean | undefined;
  statusText: string | undefined;
  onResync: (() => void) | undefined;
  isPlaying: boolean;
  audioTrackCount: number;
  onUserGesture: () => void;
  stalledCodec: string | null | undefined;
  visualizerStream: MediaStream | null;
  playerRef: React.RefObject<HTMLDivElement | null>;
  overlayClass: string;
  announcement: string | null;
}> = ({
  showWaiting,
  showGesture,
  showStall,
  statusText,
  onResync,
  isPlaying,
  audioTrackCount,
  onUserGesture,
  stalledCodec,
  visualizerStream,
  playerRef,
  overlayClass,
  announcement,
}) => (
  <>
    {showWaiting && <WaitingOverlay statusText={statusText} onResync={onResync} />}
    {showGesture && (
      <GestureOverlay isPlaying={isPlaying} audioTrackCount={audioTrackCount} onUserGesture={onUserGesture} />
    )}
    {showStall && <DecoderStallOverlay stalledCodec={stalledCodec} onResync={onResync} />}
    {visualizerStream && (
      <AudioVisualizer
        mediaStream={visualizerStream}
        playerRef={playerRef}
        showStatus
        className={`absolute z-20 hidden transition-opacity duration-300 sm:flex ${overlayClass}`}
      />
    )}
    {announcement && (
      <span role="status" aria-live="polite" className="sr-only">
        {announcement}
      </span>
    )}
  </>
);

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
  const liveVideoRef = useRef<HTMLVideoElement | null>(null);
  const replayVideoRef = useRef<HTMLVideoElement | null>(null);
  const captureVideoRef = useRef<HTMLVideoElement | null>(null);
  const isFullscreen = propIsFullscreen ?? false;
  const [isTelemetryVisible, setIsTelemetryVisible] = useState(readTelemetryVisibility);
  const replay = useViewerReplay({ mediaStream, isLive, replayVideoRef, captureVideoRef });
  const { telemetry } = useSpectatorTelemetry(isTelemetryVisible, isLive, getStatsFn, mediaStream);
  const {
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
  } = usePlaybackControls(mediaStream, fullBleed, liveVideoRef, replayVideoRef, replay);
  usePlaybackDiagnostics(liveVideoRef);

  const handleTelemetryVisibilityChange = (isVisible: boolean): void => {
    setIsTelemetryVisible(isVisible);
    saveTelemetryVisibility(isVisible);
  };
  const overlayControlsClass = getOverlayClass(isFullscreen, showFullscreenControls);
  const hasReplay = replay.snapshot.availability === 'available' && replay.snapshot.range != null;
  const isShowingReplay = replay.snapshot.mode === 'replay' && hasReplay;
  const showWaiting = (!isLive || !hasVideoTrack) && !hasReplay;
  const showGesture = needsUserGesture && isLive && hasVideoTrack && !isShowingReplay;
  const showStall = decoderStalled && isLive && hasVideoTrack && !isShowingReplay;
  const visualizerStream = isLive && mediaStream && !isShowingReplay ? mediaStream : null;
  const containerClass = fullBleed
    ? 'h-screen max-h-screen'
    : 'aspect-video rounded-2xl overflow-hidden border border-border';
  const liveVideoClass = replay.snapshot.mode === 'live' && hasVideoTrack && isLive ? 'visible' : 'invisible';
  const replayVideoClass = isShowingReplay ? 'visible' : 'invisible';
  const cursorClass = isFullscreen && !showFullscreenControls ? 'cursor-none' : '';

  return (
    <div
      ref={containerRef}
      className={`relative w-full bg-black select-none flex items-center justify-center group ${cursorClass} ${containerClass}`}
    >
      <VideoLayers
        liveVideoRef={liveVideoRef}
        replayVideoRef={replayVideoRef}
        captureVideoRef={captureVideoRef}
        liveVideoClass={liveVideoClass}
        replayVideoClass={replayVideoClass}
        onToggleFullscreen={toggleFullscreen}
      />
      <PlayerOverlays
        showWaiting={showWaiting}
        showGesture={showGesture}
        showStall={showStall}
        statusText={statusText}
        onResync={onResync}
        isPlaying={isPlaying}
        audioTrackCount={audioTrackCount}
        onUserGesture={handleUserGesture}
        stalledCodec={stalledCodec}
        visualizerStream={visualizerStream}
        playerRef={containerRef}
        overlayClass={overlayControlsClass}
        announcement={replay.snapshot.announcement}
      />

      <MediaControls
        telemetry={telemetry}
        isTelemetryVisible={isTelemetryVisible}
        onTelemetryVisibilityChange={handleTelemetryVisibilityChange}
        replay={replay}
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
