import type { AudioApp } from '@slopcast/shared-types';
import { type Room, Track } from 'livekit-client';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { notify } from '../lib/toast';
import type { CaptureContext } from '../types';
import { findCaptureAudioDevice } from '../utils/audio-devices';
import { groupAudioApps } from '../utils/audio-grouping';
import { audioWaveStore } from '../utils/audio-level-store';
import { createPcmAudioTrack } from '../utils/pcm-audio';

const AUDIO_APPS_POLL_MS = 3000;

export const findBestAudioMatch = (apps: AudioApp[], query: string): AudioApp | null => {
  const q = query.trim().toLowerCase();
  if (!q) return null;

  const qClean = q.replace(/[^a-z0-9]/g, '');

  // 1. Exact name match
  let best = apps.find((a) => a.name.trim().toLowerCase() === q);
  if (best) return best;

  // 2. Cleaned name equality (handling spaces / .exe, e.g. "zenless zone zero" vs "zenlesszonezero.exe")
  if (qClean.length >= 3) {
    best = apps.find((a) => {
      const aClean = a.name
        .trim()
        .toLowerCase()
        .replace(/[^a-z0-9]/g, '')
        .replace(/exe$/, '');
      return aClean.length >= 3 && (aClean === qClean || qClean.includes(aClean) || aClean.includes(qClean));
    });
    if (best) return best;
  }

  // 3. Name contained in query or query in name
  best = apps.find((a) => q.includes(a.name.toLowerCase()) || a.name.toLowerCase().includes(q));
  if (best) return best;

  // 4. Acronym match for multi-word queries (e.g. "Final Fantasy XIV" -> "ffxiv" matching "ffxiv_dx11.exe")
  const words = q.split(/[^a-z0-9]+/).filter((w) => w.length > 0);
  if (words.length >= 2) {
    const acronym = words.map((w) => w[0]).join('');
    if (acronym.length >= 3) {
      best = apps.find((a) => a.name.toLowerCase().includes(acronym));
      if (best) return best;
    }
  }

  // 5. Significant word match (words with length >= 4)
  for (const word of words) {
    if (word.length >= 4) {
      best = apps.find((a) => a.name.toLowerCase().includes(word));
      if (best) return best;
    }
  }

  // 6. First word match
  const firstWord = words[0];
  if (firstWord) {
    best = apps.find((a) => a.name.toLowerCase().includes(firstWord) || firstWord.includes(a.name.toLowerCase()));
    if (best) return best;
  }

  return null;
};

export interface UseAudioCaptureReturn {
  audioApps: AudioApp[];
  audioAppGroups: ReturnType<typeof groupAudioApps>;
  selectedAudioAppId: number | null;
  setSelectedAudioAppId: React.Dispatch<React.SetStateAction<number | null>>;
  audioAppExplicitlySet: boolean;
  setAudioAppExplicitlySet: React.Dispatch<React.SetStateAction<boolean>>;
  autoDetectedApp: AudioApp | null;
  setAutoDetectedApp: React.Dispatch<React.SetStateAction<AudioApp | null>>;
  autoDetectFailed: boolean;
  setAutoDetectFailed: React.Dispatch<React.SetStateAction<boolean>>;
  captureContext: CaptureContext | null;
  setCaptureContext: React.Dispatch<React.SetStateAction<CaptureContext | null>>;
  audioAppIdRef: React.RefObject<number | null>;
  loadAudioApps: () => Promise<void>;
  captureAudioTrack: (targetId: number) => Promise<MediaStreamTrack | null>;
  replaceAudioTrack: (targetId: number, room: Room | null) => Promise<void>;
  attemptAutoResolve: (opts?: { sourceId?: string; nameHint?: string }) => Promise<AudioApp | null>;
  handleSelectApp: (appId: number | null, explicit?: boolean) => void;
}

export function useAudioCapture(
  isSharing: boolean,
  liveKitRoomRef: React.RefObject<Room | null>,
  localStreamRef: React.RefObject<MediaStream | null>,
): UseAudioCaptureReturn {
  const [audioApps, setAudioApps] = useState<AudioApp[]>([]);
  const audioAppGroups = useMemo(() => groupAudioApps(audioApps), [audioApps]);
  const [selectedAudioAppId, setSelectedAudioAppId] = useState<number | null>(null);
  const [audioAppExplicitlySet, setAudioAppExplicitlySet] = useState(false);
  const [autoDetectedApp, setAutoDetectedApp] = useState<AudioApp | null>(null);
  const [captureContext, setCaptureContext] = useState<CaptureContext | null>(null);
  const [autoDetectFailed, setAutoDetectFailed] = useState(false);

  const audioAppIdRef = useRef<number | null>(null);
  const hasAudioAppsRef = useRef(false);

  // Keep the list identity stable across polls so memoized consumers
  // (AudioAppPicker, audioAppGroups) don't re-render on unchanged data.
  const loadAudioApps = useCallback(async () => {
    if (!window.electronAPI) return;
    const apps = await window.electronAPI.getAudioApps();
    setAudioApps((prev) => {
      const same =
        prev.length === apps.length && prev.every((app, i) => app.id === apps[i]?.id && app.name === apps[i]?.name);
      return same ? prev : apps;
    });
  }, []);

  // Audio apps auto-refresh
  useEffect(() => {
    hasAudioAppsRef.current = audioApps.length > 0;
  }, [audioApps.length]);

  useEffect(() => {
    void loadAudioApps();
    const interval = setInterval(() => {
      if (document.visibilityState === 'visible' && (!isSharing || !hasAudioAppsRef.current)) {
        void loadAudioApps();
      }
    }, AUDIO_APPS_POLL_MS);
    return () => clearInterval(interval);
  }, [loadAudioApps, isSharing]);

  // Audio waveform metering
  useEffect(() => {
    const api = window.electronAPI;
    if (!api?.startAudioMetering) return;

    let cancelled = false;
    let cleanupListener: (() => void) | null = null;

    const handleWave = (waves: Array<{ id: number; columns: number[] }>) => {
      if (cancelled) return;
      audioWaveStore.updateWave(waves);
    };

    void api.startAudioMetering().then((started) => {
      if (!started || cancelled) return;
      cleanupListener = api.onAudioWave?.(handleWave) ?? null;
    });

    return () => {
      cancelled = true;
      if (cleanupListener) cleanupListener();
      void api.stopAudioMetering().catch((err) => console.warn('[useAudioCapture] stopAudioMetering failed:', err));
    };
  }, []);

  const captureAudioTrack = useCallback(async (targetId: number): Promise<MediaStreamTrack | null> => {
    const started = await window.electronAPI?.startAudioCapture(targetId);
    if (!started) {
      throw new Error('Native audio capture failed to start');
    }

    const unlock = await navigator.mediaDevices.getUserMedia({ audio: true }).catch((err) => {
      console.warn('[useAudioCapture] mic permission denied; device labels may stay hidden:', err);
      return null;
    });
    for (const t of unlock?.getTracks() ?? []) {
      t.stop();
    }

    for (let attempt = 0; attempt < 4; attempt++) {
      const device = await findCaptureAudioDevice();
      if (device) {
        const stream = await navigator.mediaDevices.getUserMedia({
          audio: {
            deviceId: { exact: device.deviceId },
            echoCancellation: false,
            noiseSuppression: false,
            autoGainControl: false,
          },
        });
        const track = stream.getAudioTracks()[0];
        if (track) {
          return track;
        }
      }
      await new Promise((resolve) => setTimeout(resolve, 250));
    }

    const pcmResult = createPcmAudioTrack();
    if (pcmResult?.track) {
      return pcmResult.track;
    }

    throw new Error('Virtual capture source did not appear as an audio input device');
  }, []);

  const replaceAudioTrack = useCallback(async (targetId: number, room: Room | null): Promise<void> => {
    if (!room) return;

    let switched = await window.electronAPI?.switchAudioCapture(targetId);
    if (!switched) {
      switched = await window.electronAPI?.startAudioCapture(targetId);
    }
    if (!switched) {
      throw new Error('Native audio target switch failed');
    }
    audioAppIdRef.current = targetId;
  }, []);

  const attemptAutoResolve = useCallback(
    async (opts?: { sourceId?: string; nameHint?: string }): Promise<AudioApp | null> => {
      if (!window.electronAPI) return null;

      let app = await window.electronAPI.resolveAudioSource(opts ?? {});

      if (!app && opts?.nameHint && audioApps.length > 0) {
        app = findBestAudioMatch(audioApps, opts.nameHint);
      }

      if (!app && window.electronAPI?.getCaptureContext) {
        const ctx = await window.electronAPI.getCaptureContext();
        setCaptureContext(ctx);
        if (ctx?.sourceType === 'monitor') {
          app = { id: -1, name: 'System Audio', processId: 0 };
        }
      }

      if (app) {
        setAutoDetectedApp(app);
        setSelectedAudioAppId(app.id);
        return app;
      }
      return null;
    },
    [audioApps],
  );

  // Starts capture for a target when sharing began video-only: captures a
  // fresh track from the virtual node and publishes it to the room.
  const startAudioMidShare = useCallback(
    async (targetId: number): Promise<void> => {
      const track = await captureAudioTrack(targetId);
      if (!track) {
        throw new Error('Audio capture produced no track');
      }
      const room = liveKitRoomRef.current;
      if (!room) {
        track.stop();
        throw new Error('Not connected to a room');
      }
      localStreamRef.current?.addTrack(track);
      await room.localParticipant.publishTrack(track, {
        source: Track.Source.ScreenShareAudio,
      });
    },
    [captureAudioTrack, liveKitRoomRef, localStreamRef],
  );

  const handleSelectApp = useCallback((appId: number | null, explicit?: boolean) => {
    setAudioAppExplicitlySet(explicit ?? true);
    setSelectedAudioAppId(appId);
    if (!explicit) setAutoDetectedApp(null);
  }, []);

  // Real-time audio source switching while sharing
  useEffect(() => {
    if (!isSharing) return;
    const newId = selectedAudioAppId;
    if (newId === null) return;

    // audioAppIdRef tracks the target the native layer actually captures, so
    // `prevId` is the real previous target, not the UI selection.
    const prevId = audioAppIdRef.current;
    if (prevId === newId) return;

    const applyAudio = async () => {
      try {
        if (prevId === null) {
          await startAudioMidShare(newId);
        } else {
          await replaceAudioTrack(newId, liveKitRoomRef.current);
        }
        audioAppIdRef.current = newId;
        setAutoDetectedApp(null);
        setAudioAppExplicitlySet(true);
      } catch (err) {
        console.error('[useAudioCapture] audio switch failed:', err);
        notify(
          'error',
          prevId === null ? 'Audio start failed' : 'Audio switch failed',
          prevId === null
            ? 'Could not start capturing the selected audio source.'
            : 'Could not switch to the selected audio source.',
        );
      }
    };
    void applyAudio();
  }, [selectedAudioAppId, isSharing, startAudioMidShare, replaceAudioTrack, liveKitRoomRef]);

  return {
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
    captureAudioTrack,
    replaceAudioTrack,
    attemptAutoResolve,
    handleSelectApp,
  };
}
