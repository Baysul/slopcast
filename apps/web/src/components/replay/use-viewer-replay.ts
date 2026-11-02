import type React from 'react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';

import { ViewerReplay, readReplayWindowSeconds } from './viewer-replay';
import type { ViewerReplaySnapshot } from './viewer-replay';

interface UseViewerReplayOptions {
  readonly mediaStream: MediaStream | null;
  readonly isLive: boolean;
  readonly liveVideoRef: React.RefObject<HTMLVideoElement | null>;
  readonly replayVideoRef: React.RefObject<HTMLVideoElement | null>;
}

export interface UseViewerReplayResult {
  snapshot: ViewerReplaySnapshot;
  setWindowSeconds: (seconds: number) => void;
  seek: (mediaTime: number, shouldPlay: boolean) => void;
  goLive: () => void;
  playReplay: () => void;
  pauseReplay: () => void;
  preview: (mediaTime: number | null) => void;
  clearAnnouncement: () => void;
}

export const useViewerReplay = ({
  mediaStream,
  isLive,
  liveVideoRef,
  replayVideoRef,
}: UseViewerReplayOptions): UseViewerReplayResult => {
  const initialWindow = useMemo(() => readReplayWindowSeconds(), []);
  const [snapshot, setSnapshot] = useState<ViewerReplaySnapshot>(() => ({
    announcement: null,
    availability: 'checking',
    effectiveWindowSeconds: initialWindow,
    isShareEnded: false,
    limitationReason: null,
    mode: 'live',
    position: 0,
    preview: null,
    range: null,
    unavailableReason: null,
    windowSeconds: initialWindow,
  }));
  const controllerRef = useRef<ViewerReplay | null>(null);
  const activeStreamRef = useRef<MediaStream | null>(null);

  useEffect(() => {
    const liveVideo = liveVideoRef.current;
    const replayVideo = replayVideoRef.current;
    if (!liveVideo || !replayVideo) {
      return undefined;
    }

    const controller = new ViewerReplay(replayVideo, liveVideo, initialWindow, setSnapshot);
    const handlePageHide = (event: PageTransitionEvent): void => {
      if (!event.persisted) {
        controller.destroy();
      }
    };
    controllerRef.current = controller;
    setSnapshot(controller.getSnapshot());
    window.addEventListener('pagehide', handlePageHide);

    return () => {
      window.removeEventListener('pagehide', handlePageHide);
      controller.destroy();
      controllerRef.current = null;
      activeStreamRef.current = null;
    };
  }, [initialWindow, liveVideoRef, replayVideoRef]);

  useEffect(() => {
    const controller = controllerRef.current;
    const hasVideo = mediaStream?.getVideoTracks().some((track) => track.readyState === 'live') ?? false;
    if (!controller) {
      return;
    }

    if (isLive && mediaStream && hasVideo) {
      if (activeStreamRef.current === mediaStream) {
        return;
      }

      activeStreamRef.current = mediaStream;
      controller.startShare(mediaStream).catch((error) => {
        console.info('[Replay] Could not start viewer-session buffering:', error);
      });
      return;
    }

    if (activeStreamRef.current) {
      activeStreamRef.current = null;
      controller.endShare();
    }
  }, [isLive, mediaStream]);

  useEffect(() => {
    const controller = controllerRef.current;
    if (!controller || !mediaStream) {
      return undefined;
    }

    const handleTrackChange = (event: MediaStreamTrackEvent): void => {
      if (event.track.kind !== 'video') {
        return undefined;
      }

      const hasVideo = mediaStream.getVideoTracks().some((track) => track.readyState === 'live');
      if (isLive && hasVideo) {
        activeStreamRef.current = mediaStream;
        controller.startShare(mediaStream).catch((error) => {
          console.info('[Replay] Could not restart buffering after a video track change:', error);
        });
        return undefined;
      }
      if (activeStreamRef.current) {
        activeStreamRef.current = null;
        controller.endShare();
      }
    };

    mediaStream.addEventListener('addtrack', handleTrackChange);
    mediaStream.addEventListener('removetrack', handleTrackChange);
    return () => {
      mediaStream.removeEventListener('addtrack', handleTrackChange);
      mediaStream.removeEventListener('removetrack', handleTrackChange);
    };
  }, [isLive, mediaStream]);

  const setWindowSeconds = useCallback((seconds: number) => {
    controllerRef.current?.setWindowSeconds(seconds);
  }, []);
  const seek = useCallback((mediaTime: number, shouldPlay: boolean) => {
    controllerRef.current?.seek(mediaTime, shouldPlay);
  }, []);
  const goLive = useCallback(() => {
    controllerRef.current?.goLive();
  }, []);
  const playReplay = useCallback(() => {
    controllerRef.current?.playReplay();
  }, []);
  const pauseReplay = useCallback(() => {
    controllerRef.current?.pauseReplay();
  }, []);
  const preview = useCallback((mediaTime: number | null) => {
    controllerRef.current?.preview(mediaTime);
  }, []);
  const clearAnnouncement = useCallback(() => {
    controllerRef.current?.clearAnnouncement();
  }, []);

  return {
    clearAnnouncement,
    goLive,
    pauseReplay,
    playReplay,
    preview,
    seek,
    setWindowSeconds,
    snapshot,
  };
};
