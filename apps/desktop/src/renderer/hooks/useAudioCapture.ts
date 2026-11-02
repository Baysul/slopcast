import type { AudioApp, AudioAppWave } from '@slopcast/shared-types';
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';

import { desktopApi } from '../api/desktop';
import { notify } from '../lib/toast';
import type { CaptureContext } from '../types';
import { audioAppsStore } from '../utils/audio-apps-store';
import { groupAudioApps } from '../utils/audio-grouping';
import { audioWaveStore } from '../utils/audio-level-store';
import { toAudioTargetId } from '../utils/audio-targets';

const cleanName = (name: string): string =>
  name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]/gu, '')
    .replace(/exe$/u, '');

const findBestAudioMatch = (apps: AudioApp[], query: string): AudioApp | null => {
  const name = cleanName(query);
  if (name.length < 3) {
    return null;
  }

  const candidates = apps.filter((app) => {
    const candidate = cleanName(app.name);
    return app.id !== -1 && candidate.length >= 3 && (name.includes(candidate) || candidate.includes(name));
  });

  return candidates.length === 1 ? (candidates[0] ?? null) : null;
};

export interface UseAudioCaptureReturn {
  audioApps: AudioApp[];
  audioAppGroups: ReturnType<typeof groupAudioApps>;
  /** The capture target id (negative pid on Linux). 0 means "no audio". */
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
  stopAudioCapture: () => Promise<boolean>;
  startAudioCapture: (targetId: number) => Promise<boolean>;
  switchAudioCapture: (targetId: number) => Promise<boolean>;
  attemptAutoResolve: (nameHint?: string) => Promise<AudioApp | null>;
  handleSelectApp: (appId: number | null, explicit?: boolean) => void;
}

const changeAudioTarget = (previousId: number | null, targetId: number): Promise<boolean> => {
  if (targetId === 0) {
    return desktopApi.stopAudioCapture();
  }
  if (previousId === null || previousId === 0) {
    return desktopApi.startAudioCapture(targetId);
  }

  return desktopApi.switchAudioCapture(targetId);
};

export const useAudioCapture = (
  isSharing: boolean,
  pidBasedTargets: boolean,
  autoSelectAudio: boolean,
): UseAudioCaptureReturn => {
  const audioApps = useSyncExternalStore(audioAppsStore.subscribe, audioAppsStore.getSnapshot);
  const audioAppGroups = useMemo(() => groupAudioApps(audioApps), [audioApps]);
  const [selectedAudioAppId, setSelectedAudioAppId] = useState<number | null>(null);
  const [audioAppExplicitlySet, setAudioAppExplicitlySet] = useState(false);
  const [autoDetectedApp, setAutoDetectedApp] = useState<AudioApp | null>(null);
  const [captureContext, setCaptureContext] = useState<CaptureContext | null>(null);
  const [autoDetectFailed, setAutoDetectFailed] = useState(false);

  const audioAppIdRef = useRef<number | null>(null);
  const audioOperationRef = useRef(Promise.resolve());
  const audioRevisionRef = useRef(0);
  const autoSelectAudioRef = useRef(autoSelectAudio);
  useEffect(() => {
    autoSelectAudioRef.current = autoSelectAudio;
  }, [autoSelectAudio]);

  const loadAudioApps = audioAppsStore.refresh;

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | null = null;

    const handleWave = (waves: AudioAppWave[]) => {
      if (cancelled) {
        return;
      }
      audioWaveStore.updateWave(waves);
    };

    void desktopApi.startAudioMetering().then((started) => {
      if (!started || cancelled) {
        return;
      }
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

  const applyAudioTarget = useCallback((targetId: number): Promise<boolean> => {
    const revision = audioRevisionRef.current;
    const operation = audioOperationRef.current.then(async () => {
      if (revision !== audioRevisionRef.current) {
        return false;
      }
      const applied = await changeAudioTarget(audioAppIdRef.current, targetId);
      if (!applied) {
        throw new Error('Native audio target change failed');
      }
      if (revision !== audioRevisionRef.current) {
        if (!(await desktopApi.stopAudioCapture())) {
          throw new Error('Cancelled audio capture could not stop');
        }
        audioAppIdRef.current = 0;
        return false;
      }
      audioAppIdRef.current = targetId;
      return true;
    });
    // Keep the queue usable after failure; the caller still receives the rejection.
    audioOperationRef.current = operation.then(
      () => undefined,
      (cause: unknown) => {
        console.error('Audio target operation failed:', cause);
      },
    );
    return operation;
  }, []);
  const stopAudioCapture = useCallback(() => {
    audioRevisionRef.current += 1;
    return applyAudioTarget(0);
  }, [applyAudioTarget]);
  const startAudioCapture = applyAudioTarget;
  const switchAudioCapture = applyAudioTarget;

  // `attemptAutoResolve` is called from event handlers, so it reads the current app list and
  // Target-id mode through a ref instead of closing over them.
  const autoResolveContextRef = useRef({ audioApps, pidBasedTargets });
  useEffect(() => {
    autoResolveContextRef.current = { audioApps, pidBasedTargets };
  }, [audioApps, pidBasedTargets]);

  const attemptAutoResolve = useCallback(async (nameHint?: string): Promise<AudioApp | null> => {
    if (!autoSelectAudioRef.current) {
      return null;
    }
    const { audioApps: apps, pidBasedTargets: pidTargets } = autoResolveContextRef.current;
    const revision = audioRevisionRef.current;
    const isCurrent = () => autoSelectAudioRef.current && revision === audioRevisionRef.current;
    let app = await desktopApi.resolveAudioSource(nameHint);
    if (!isCurrent()) {
      return null;
    }
    if (app?.id === -1) {
      app = null;
    }

    if (nameHint !== undefined && nameHint.length > 0) {
      app ??= findBestAudioMatch(apps, nameHint);
    }

    if (!app) {
      const ctx = await desktopApi.getCaptureContext();
      if (!isCurrent()) {
        return null;
      }
      setCaptureContext(ctx);
      setAutoDetectFailed(true);
    }

    if (app) {
      setAutoDetectedApp(app);
      setSelectedAudioAppId(toAudioTargetId(app, pidTargets));
      return app;
    }
    return null;
  }, []);

  // Reset the auto-detected selection during render, as React recommends for state derived from
  // Props, instead of re-rendering from an effect. The native stop stays in the effect below.
  const [audioPreferenceKey, setAudioPreferenceKey] = useState({ audioAppExplicitlySet, autoSelectAudio });
  if (
    audioPreferenceKey.audioAppExplicitlySet !== audioAppExplicitlySet ||
    audioPreferenceKey.autoSelectAudio !== autoSelectAudio
  ) {
    setAudioPreferenceKey({ audioAppExplicitlySet, autoSelectAudio });
    if (!audioAppExplicitlySet) {
      setSelectedAudioAppId(autoSelectAudio ? null : 0);
      setAutoDetectedApp(null);
      setAutoDetectFailed(false);
    }
  }

  useEffect(() => {
    if (audioAppExplicitlySet) {
      return undefined;
    }
    audioRevisionRef.current += 1;
    if (!autoSelectAudio) {
      void stopAudioCapture().catch((cause: unknown) => {
        notify('error', 'Audio stop failed', String(cause));
      });
    }
    return undefined;
  }, [autoSelectAudio, audioAppExplicitlySet, stopAudioCapture]);

  const handleSelectApp = useCallback(
    (appId: number | null, explicit?: boolean) => {
      audioRevisionRef.current += 1;
      setAudioAppExplicitlySet(explicit ?? true);
      const app = audioApps.find((a) => a.id === appId) ?? null;
      setSelectedAudioAppId(app ? toAudioTargetId(app, pidBasedTargets) : appId);
      if (explicit !== true) {
        setAutoDetectedApp(null);
      }
    },
    [audioApps, pidBasedTargets],
  );

  useEffect(() => {
    if (!isSharing) {
      return undefined;
    }
    const newId = selectedAudioAppId;
    if (newId === null) {
      return undefined;
    }

    const prevId = audioAppIdRef.current;
    if (prevId === newId) {
      return undefined;
    }

    let cancelled = false;
    const applyAudio = async () => {
      try {
        await applyAudioTarget(newId);
      } catch (err) {
        if (cancelled) {
          return;
        }
        setSelectedAudioAppId(audioAppIdRef.current);
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
    return () => {
      cancelled = true;
    };
  }, [selectedAudioAppId, isSharing, applyAudioTarget]);

  return {
    attemptAutoResolve,
    audioAppExplicitlySet,
    audioAppGroups,
    audioAppIdRef,
    audioApps,
    autoDetectFailed,
    autoDetectedApp,
    captureContext,
    handleSelectApp,
    loadAudioApps,
    selectedAudioAppId,
    setAudioAppExplicitlySet,
    setAutoDetectFailed,
    setAutoDetectedApp,
    setCaptureContext,
    setSelectedAudioAppId,
    startAudioCapture,
    stopAudioCapture,
    switchAudioCapture,
  };
};
