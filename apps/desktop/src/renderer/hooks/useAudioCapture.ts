import type { AudioApp, AudioAppWave } from '@slopcast/shared-types';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { desktopApi } from '../api/desktop';
import { notify } from '../lib/toast';
import type { CaptureContext } from '../types';
import { audioAppsEqual, groupAudioApps } from '../utils/audio-grouping';
import { audioWaveStore } from '../utils/audio-level-store';
import { toAudioTargetId } from '../utils/audio-targets';

const AUDIO_APPS_POLL_MS = 3000;

const cleanName = (name: string): string =>
  name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]/g, '')
    .replace(/exe$/, '');

const findBestAudioMatch = (apps: AudioApp[], query: string): AudioApp | null => {
  const name = cleanName(query);
  if (name.length < 3) return null;

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

async function changeAudioTarget(previousId: number | null, targetId: number): Promise<boolean> {
  if (targetId === 0) return desktopApi.stopAudioCapture();
  if (previousId === null || previousId === 0) return desktopApi.startAudioCapture(targetId);

  return desktopApi.switchAudioCapture(targetId);
}

export function useAudioCapture(
  isSharing: boolean,
  pidBasedTargets: boolean,
  autoSelectAudio: boolean,
): UseAudioCaptureReturn {
  const [audioApps, setAudioApps] = useState<AudioApp[]>([]);
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
  autoSelectAudioRef.current = autoSelectAudio;

  const loadAudioApps = useCallback(async () => {
    const apps = await desktopApi.getAudioApps();
    setAudioApps((prev) => (audioAppsEqual(prev, apps) ? prev : apps));
  }, []);

  useEffect(() => {
    void loadAudioApps();
    const interval = setInterval(() => {
      if (document.visibilityState === 'visible') {
        void loadAudioApps();
      }
    }, AUDIO_APPS_POLL_MS);
    return () => clearInterval(interval);
  }, [loadAudioApps]);

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

  const applyAudioTarget = useCallback((targetId: number): Promise<boolean> => {
    const revision = audioRevisionRef.current;
    const operation = audioOperationRef.current.then(async () => {
      if (revision !== audioRevisionRef.current) return false;
      const applied = await changeAudioTarget(audioAppIdRef.current, targetId);
      if (!applied) throw new Error('Native audio target change failed');
      if (revision !== audioRevisionRef.current) {
        if (!(await desktopApi.stopAudioCapture())) throw new Error('Cancelled audio capture could not stop');
        audioAppIdRef.current = 0;
        return false;
      }
      audioAppIdRef.current = targetId;
      return true;
    });
    // Keep the queue usable after failure; the caller still receives the rejection.
    audioOperationRef.current = operation.then(
      () => undefined,
      (cause: unknown) => console.error('Audio target operation failed:', cause),
    );
    return operation;
  }, []);
  const stopAudioCapture = useCallback(() => {
    audioRevisionRef.current += 1;
    return applyAudioTarget(0);
  }, [applyAudioTarget]);
  const startAudioCapture = applyAudioTarget;
  const switchAudioCapture = applyAudioTarget;

  const attemptAutoResolve = useCallback(
    async (nameHint?: string): Promise<AudioApp | null> => {
      if (!autoSelectAudioRef.current) return null;
      const revision = audioRevisionRef.current;
      const isCurrent = () => autoSelectAudioRef.current && revision === audioRevisionRef.current;
      let app = await desktopApi.resolveAudioSource(nameHint);
      if (!isCurrent()) return null;
      if (app?.id === -1) app = null;

      if (nameHint) app ??= findBestAudioMatch(audioApps, nameHint);

      if (!app) {
        const ctx = await desktopApi.getCaptureContext();
        if (!isCurrent()) return null;
        setCaptureContext(ctx);
        setAutoDetectFailed(true);
      }

      if (app) {
        setAutoDetectedApp(app);
        setSelectedAudioAppId(toAudioTargetId(app, pidBasedTargets));
        return app;
      }
      return null;
    },
    [audioApps, pidBasedTargets],
  );

  useEffect(() => {
    if (audioAppExplicitlySet) return;
    audioRevisionRef.current += 1;
    if (!autoSelectAudio) {
      void stopAudioCapture().catch((cause: unknown) => {
        notify('error', 'Audio stop failed', String(cause));
      });
    }
    setSelectedAudioAppId(autoSelectAudio ? null : 0);
    setAutoDetectedApp(null);
    setAutoDetectFailed(false);
  }, [autoSelectAudio, audioAppExplicitlySet, stopAudioCapture]);

  const handleSelectApp = useCallback(
    (appId: number | null, explicit?: boolean) => {
      audioRevisionRef.current += 1;
      setAudioAppExplicitlySet(explicit ?? true);
      const app = audioApps.find((a) => a.id === appId) ?? null;
      setSelectedAudioAppId(app ? toAudioTargetId(app, pidBasedTargets) : appId);
      if (!explicit) setAutoDetectedApp(null);
    },
    [audioApps, pidBasedTargets],
  );

  useEffect(() => {
    if (!isSharing) return;
    const newId = selectedAudioAppId;
    if (newId === null) return;

    const prevId = audioAppIdRef.current;
    if (prevId === newId) return;

    let cancelled = false;
    const applyAudio = async () => {
      try {
        await applyAudioTarget(newId);
      } catch (err) {
        if (cancelled) return;
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
    stopAudioCapture,
    startAudioCapture,
    switchAudioCapture,
    attemptAutoResolve,
    handleSelectApp,
  };
}
