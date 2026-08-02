import type { AudioApp } from '@slopcast/shared-types';
import type { Room } from 'livekit-client';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { notify } from '../lib/toast';
import type { CaptureContext } from '../types';
import { findCaptureAudioDevice } from '../utils/audio-devices';
import { groupAudioApps } from '../utils/audio-grouping';
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
  audioLevels: ReadonlyMap<number, number>;
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
): UseAudioCaptureReturn {
  const [audioApps, setAudioApps] = useState<AudioApp[]>([]);
  const [audioLevels, setAudioLevels] = useState<ReadonlyMap<number, number>>(new Map());
  const audioAppGroups = useMemo(() => groupAudioApps(audioApps), [audioApps]);
  const [selectedAudioAppId, setSelectedAudioAppId] = useState<number | null>(null);
  const [audioAppExplicitlySet, setAudioAppExplicitlySet] = useState(false);
  const [autoDetectedApp, setAutoDetectedApp] = useState<AudioApp | null>(null);
  const [captureContext, setCaptureContext] = useState<CaptureContext | null>(null);
  const [autoDetectFailed, setAutoDetectFailed] = useState(false);

  const audioAppIdRef = useRef<number | null>(null);

  useEffect(() => {
    audioAppIdRef.current = selectedAudioAppId;
  }, [selectedAudioAppId]);

  const loadAudioApps = useCallback(async () => {
    if (window.electronAPI) {
      const apps = await window.electronAPI.getAudioApps();
      setAudioApps(apps);
    }
  }, []);

  // Audio apps auto-refresh
  useEffect(() => {
    void loadAudioApps();
    const interval = setInterval(() => {
      if (document.visibilityState === 'visible' && (!isSharing || audioApps.length === 0)) {
        void loadAudioApps();
      }
    }, AUDIO_APPS_POLL_MS);
    return () => clearInterval(interval);
  }, [loadAudioApps, isSharing, audioApps.length]);

  // Audio level metering
  useEffect(() => {
    const api = window.electronAPI;
    if (!api?.startAudioMetering) return;

    let cancelled = false;
    let interval: ReturnType<typeof setInterval> | null = null;

    void api.startAudioMetering().then((started) => {
      if (!started || cancelled) return;
      interval = setInterval(() => {
        if (document.visibilityState !== 'visible') return;
        void api.getAudioLevels().then((levels) => {
          if (cancelled) return;
          setAudioLevels((prev) => {
            let changed = false;
            const next = new Map<number, number>();
            for (const { id, level } of levels) {
              const prevLevel = prev.get(id) ?? 0;
              const decayed = Math.max(level, prevLevel * 0.82);
              next.set(id, decayed);
              if (Math.abs(decayed - prevLevel) > 0.005) {
                changed = true;
              }
            }
            if (prev.size !== next.size) {
              changed = true;
            }
            if (!changed) return prev;
            return next;
          });
        });
      }, 100);
    });

    return () => {
      cancelled = true;
      if (interval) clearInterval(interval);
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

  const handleSelectApp = useCallback((appId: number | null, explicit?: boolean) => {
    setAudioAppExplicitlySet(explicit ?? true);
    setSelectedAudioAppId(appId);
    if (!explicit) setAutoDetectedApp(null);
  }, []);

  // Real-time audio source switching while sharing
  useEffect(() => {
    if (!isSharing) return;
    if (selectedAudioAppId === null) return;

    const prevId = audioAppIdRef.current;
    if (prevId === selectedAudioAppId) return;
    if (prevId == null && selectedAudioAppId != null) {
      audioAppIdRef.current = selectedAudioAppId;
      return;
    }

    const switchAudio = async () => {
      try {
        await replaceAudioTrack(selectedAudioAppId, liveKitRoomRef.current);
        setSelectedAudioAppId(selectedAudioAppId);
        setAutoDetectedApp(null);
        setAudioAppExplicitlySet(true);
      } catch (err) {
        console.error('[useAudioCapture] audio switch failed:', err);
        notify('error', 'Audio switch failed', 'Could not switch to the selected audio source.');
      }
    };
    void switchAudio();
  }, [selectedAudioAppId, isSharing, replaceAudioTrack, liveKitRoomRef]);

  return {
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
  };
}
