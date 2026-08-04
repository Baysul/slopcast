import type { AudioApp, AudioAppWave } from '@slopcast/shared-types';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { desktopApi } from '../api/desktop';
import { notify } from '../lib/toast';
import type { CaptureContext } from '../types';
import { groupAudioApps } from '../utils/audio-grouping';
import { audioWaveStore } from '../utils/audio-level-store';

const AUDIO_APPS_POLL_MS = 3000;

const wordsOf = (q: string): string[] => q.split(/[^a-z0-9]+/).filter((w) => w.length > 0);

// 1. Exact name match
const matchExactName = (apps: AudioApp[], q: string): AudioApp | null =>
  apps.find((a) => a.name.trim().toLowerCase() === q) ?? null;

const cleanName = (name: string): string =>
  name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]/g, '')
    .replace(/exe$/, '');

// 2. Cleaned name equality (handling spaces / .exe, e.g. "zenless zone zero" vs "zenlesszonezero.exe")
const matchCleanedName = (apps: AudioApp[], qClean: string): AudioApp | null => {
  if (qClean.length < 3) return null;
  return (
    apps.find((a) => {
      const aClean = cleanName(a.name);
      return aClean.length >= 3 && (aClean === qClean || qClean.includes(aClean) || aClean.includes(qClean));
    }) ?? null
  );
};

// 3. Name contained in query or query in name
const matchNameContained = (apps: AudioApp[], q: string): AudioApp | null => {
  const qLower = q.toLowerCase();
  return apps.find((a) => qLower.includes(a.name.toLowerCase()) || a.name.toLowerCase().includes(qLower)) ?? null;
};

// 4. Acronym match for multi-word queries (e.g. "Final Fantasy XIV" -> "ffxiv" matching "ffxiv_dx11.exe")
const matchAcronym = (apps: AudioApp[], q: string): AudioApp | null => {
  const words = wordsOf(q);
  if (words.length < 2) return null;
  const acronym = words.map((w) => w[0]).join('');
  if (acronym.length < 3) return null;
  return apps.find((a) => a.name.toLowerCase().includes(acronym)) ?? null;
};

// 5. Significant word match (words with length >= 4)
const matchSignificantWord = (apps: AudioApp[], q: string): AudioApp | null => {
  for (const word of wordsOf(q)) {
    if (word.length >= 4) {
      const best = apps.find((a) => a.name.toLowerCase().includes(word));
      if (best) return best;
    }
  }
  return null;
};

// 6. First word match
const matchFirstWord = (apps: AudioApp[], q: string): AudioApp | null => {
  const firstWord = wordsOf(q)[0];
  if (!firstWord) return null;
  return apps.find((a) => a.name.toLowerCase().includes(firstWord) || firstWord.includes(a.name.toLowerCase())) ?? null;
};

export const findBestAudioMatch = (apps: AudioApp[], query: string): AudioApp | null => {
  const q = query.trim().toLowerCase();
  if (!q) return null;

  const qClean = q.replace(/[^a-z0-9]/g, '');
  return (
    matchExactName(apps, q) ??
    matchCleanedName(apps, qClean) ??
    matchNameContained(apps, q) ??
    matchAcronym(apps, q) ??
    matchSignificantWord(apps, q) ??
    matchFirstWord(apps, q)
  );
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
  startAudioCapture: (targetId: number) => Promise<boolean>;
  switchAudioCapture: (targetId: number) => Promise<boolean>;
  attemptAutoResolve: (nameHint?: string) => Promise<AudioApp | null>;
  handleSelectApp: (appId: number | null, explicit?: boolean) => void;
}

export function useAudioCapture(isSharing: boolean): UseAudioCaptureReturn {
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
    const apps = await desktopApi.getAudioApps();
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
    let cancelled = false;
    let unlisten: (() => void) | null = null;

    const handleWave = (waves: AudioAppWave[]) => {
      if (cancelled) return;
      audioWaveStore.updateWave(waves);
    };

    void desktopApi.startAudioMetering().then((started) => {
      if (!started || cancelled) return;
      void desktopApi.onAudioWave(handleWave).then((un) => {
        if (cancelled) {
          un();
          return;
        }
        unlisten = un;
      });
    });

    return () => {
      cancelled = true;
      unlisten?.();
      void desktopApi.stopAudioMetering();
    };
  }, []);

  // Selects the app whose PipeWire streams get linked into the capture node.
  // The PCM then flows native-rust -> backend -> native-livekit automatically;
  // the renderer never creates or publishes a JS audio track.
  const startAudioCapture = useCallback(async (targetId: number): Promise<boolean> => {
    const started = await desktopApi.startAudioCapture(targetId);
    if (!started) {
      throw new Error('Native audio capture failed to start');
    }
    audioAppIdRef.current = targetId;
    return true;
  }, []);

  const switchAudioCapture = useCallback(async (targetId: number): Promise<boolean> => {
    let switched = await desktopApi.switchAudioCapture(targetId);
    if (!switched) {
      switched = await desktopApi.startAudioCapture(targetId);
    }
    if (!switched) {
      throw new Error('Native audio target switch failed');
    }
    audioAppIdRef.current = targetId;
    return true;
  }, []);

  const attemptAutoResolve = useCallback(
    async (nameHint?: string): Promise<AudioApp | null> => {
      let app = await desktopApi.resolveAudioSource(nameHint);

      if (!app && nameHint && audioApps.length > 0) {
        app = findBestAudioMatch(audioApps, nameHint);
      }

      if (!app) {
        const ctx = await desktopApi.getCaptureContext();
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
    const newId = selectedAudioAppId;
    if (newId === null) return;

    // audioAppIdRef tracks the target the native layer actually captures, so
    // `prevId` is the real previous target, not the UI selection.
    const prevId = audioAppIdRef.current;
    if (prevId === newId) return;

    const applyAudio = async () => {
      try {
        if (prevId === null) {
          await startAudioCapture(newId);
        } else {
          await switchAudioCapture(newId);
        }
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
  }, [selectedAudioAppId, isSharing, startAudioCapture, switchAudioCapture]);

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
    startAudioCapture,
    switchAudioCapture,
    attemptAutoResolve,
    handleSelectApp,
  };
}
