import { DEFAULT_STREAM_SETTINGS } from '@slopcast/shared-types';
import type { MotionMode, ResolutionPreset, StreamSettings, VideoCodec } from '@slopcast/shared-types';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';

import { desktopApi } from '../api/desktop';
import { notify } from '../lib/toast';
import { fromNativeCodecInfo, recommendCodec } from '../utils/codecs';
import type { CodecInfo } from '../utils/codecs';

const SETTINGS_SAVE_DEBOUNCE_MS = 800;

const mergeSavedValue = <T>(current: T, previous: T | undefined, saved: T): T => {
  if (previous !== undefined && current !== previous) {
    return current;
  }

  return saved;
};

const streamSettingsEqual = (a: StreamSettings, b: StreamSettings): boolean =>
  a.fps === b.fps &&
  a.bitrateLimit === b.bitrateLimit &&
  a.videoCodec === b.videoCodec &&
  a.resolution === b.resolution &&
  a.apiEndpoint === b.apiEndpoint &&
  a.apiEndpointIsCustom === b.apiEndpointIsCustom &&
  a.pendingApiEndpoint === b.pendingApiEndpoint &&
  a.autoBitrate === b.autoBitrate &&
  a.motionMode === b.motionMode;

export interface UseStreamSettingsReturn {
  apiEndpoint: string;
  activateApiEndpoint: (endpoint: string) => void;
  pendingApiEndpoint: string | null;
  setPendingApiEndpoint: React.Dispatch<React.SetStateAction<string | null>>;
  livekitUrl: string;
  setLivekitUrl: React.Dispatch<React.SetStateAction<string>>;
  streamFps: number;
  setStreamFps: React.Dispatch<React.SetStateAction<number>>;
  bitrateLimit: number;
  setBitrateLimit: React.Dispatch<React.SetStateAction<number>>;
  availableCodecs: CodecInfo[];
  setAvailableCodecs: React.Dispatch<React.SetStateAction<CodecInfo[]>>;
  videoCodec: VideoCodec;
  setVideoCodec: React.Dispatch<React.SetStateAction<VideoCodec>>;
  resolution: ResolutionPreset;
  setResolution: React.Dispatch<React.SetStateAction<ResolutionPreset>>;
  autoBitrate: boolean;
  setAutoBitrate: React.Dispatch<React.SetStateAction<boolean>>;
  motionMode: MotionMode;
  setMotionMode: React.Dispatch<React.SetStateAction<MotionMode>>;
  streamFpsRef: React.RefObject<number>;
  bitrateLimitRef: React.RefObject<number>;
  resolutionRef: React.RefObject<ResolutionPreset>;
  autoBitrateRef: React.RefObject<boolean>;
  settingsHydrated: boolean;
}

export const useStreamSettings = (): UseStreamSettingsReturn => {
  const [apiEndpoint, setApiEndpoint] = useState<string>('http://localhost:3001');
  const [apiEndpointIsCustom, setApiEndpointIsCustom] = useState(false);
  const [pendingApiEndpoint, setPendingApiEndpoint] = useState<string | null>(null);
  const [livekitUrl, setLivekitUrl] = useState<string>('');
  const [streamFps, setStreamFps] = useState(DEFAULT_STREAM_SETTINGS.fps);
  const [bitrateLimit, setBitrateLimit] = useState(DEFAULT_STREAM_SETTINGS.bitrateLimit);
  const [availableCodecs, setAvailableCodecs] = useState<CodecInfo[]>([]);
  const [videoCodec, setVideoCodec] = useState<VideoCodec>(DEFAULT_STREAM_SETTINGS.videoCodec);
  const [resolution, setResolution] = useState<ResolutionPreset>(DEFAULT_STREAM_SETTINGS.resolution);
  const [autoBitrate, setAutoBitrate] = useState<boolean>(DEFAULT_STREAM_SETTINGS.autoBitrate);
  const [motionMode, setMotionMode] = useState<MotionMode>(DEFAULT_STREAM_SETTINGS.motionMode);
  const [settingsHydrated, setSettingsHydrated] = useState(false);
  const streamFpsRef = useRef(streamFps);
  const bitrateLimitRef = useRef(bitrateLimit);
  const resolutionRef = useRef(resolution);
  const autoBitrateRef = useRef(autoBitrate);
  const lastSavedSettingsRef = useRef<StreamSettings | null>(null);
  const configEndpointRef = useRef<string>('');
  const codecsRef = useRef(availableCodecs);
  const currentSettings: StreamSettings = useMemo(
    () => ({
      apiEndpoint,
      apiEndpointIsCustom,
      autoBitrate,
      bitrateLimit,
      fps: streamFps,
      motionMode,
      pendingApiEndpoint,
      resolution,
      videoCodec,
    }),
    [
      apiEndpoint,
      apiEndpointIsCustom,
      autoBitrate,
      bitrateLimit,
      streamFps,
      motionMode,
      pendingApiEndpoint,
      resolution,
      videoCodec,
    ],
  );
  const currentSettingsRef = useRef(currentSettings);
  useEffect(() => {
    currentSettingsRef.current = currentSettings;
  }, [currentSettings]);

  const applySavedSettings = useCallback((saved: StreamSettings) => {
    const { current } = currentSettingsRef;
    const previous = lastSavedSettingsRef.current;
    const codecs = codecsRef.current;
    const isSupported = codecs.some((codec) => codec.codec === saved.videoCodec);
    const bestCodec = codecs[0]?.codec ?? 'vp8';
    const nextVideoCodec = isSupported ? saved.videoCodec : bestCodec;
    let nextApiEndpoint = configEndpointRef.current;
    if (saved.apiEndpointIsCustom || !nextApiEndpoint) {
      nextApiEndpoint = saved.apiEndpoint;
    }
    const hydrated = { ...saved, apiEndpoint: nextApiEndpoint, videoCodec: nextVideoCodec };

    // Incoming saves must not discard edits that this window has not saved yet.
    lastSavedSettingsRef.current = hydrated;
    setStreamFps(mergeSavedValue(current.fps, previous?.fps, hydrated.fps));
    setBitrateLimit(mergeSavedValue(current.bitrateLimit, previous?.bitrateLimit, hydrated.bitrateLimit));
    setVideoCodec(mergeSavedValue(current.videoCodec, previous?.videoCodec, hydrated.videoCodec));
    setResolution(mergeSavedValue(current.resolution, previous?.resolution, hydrated.resolution));
    setApiEndpoint(mergeSavedValue(current.apiEndpoint, previous?.apiEndpoint, hydrated.apiEndpoint));
    setApiEndpointIsCustom(
      mergeSavedValue(current.apiEndpointIsCustom, previous?.apiEndpointIsCustom, hydrated.apiEndpointIsCustom),
    );
    setPendingApiEndpoint(
      mergeSavedValue(current.pendingApiEndpoint, previous?.pendingApiEndpoint, hydrated.pendingApiEndpoint),
    );
    setAutoBitrate(mergeSavedValue(current.autoBitrate, previous?.autoBitrate, hydrated.autoBitrate));
    setMotionMode(mergeSavedValue(current.motionMode, previous?.motionMode, hydrated.motionMode));
  }, []);

  useEffect(() => {
    streamFpsRef.current = streamFps;
  }, [streamFps]);
  useEffect(() => {
    bitrateLimitRef.current = bitrateLimit;
  }, [bitrateLimit]);
  useEffect(() => {
    resolutionRef.current = resolution;
  }, [resolution]);
  useEffect(() => {
    autoBitrateRef.current = autoBitrate;
  }, [autoBitrate]);

  useEffect(() => {
    const lifecycle = { disposed: false };
    const unlisten = desktopApi.onStreamSettingsChanged((saved) => {
      if (!lifecycle.disposed) {
        applySavedSettings(saved);
      }
    });
    void (async () => {
      await unlisten;
      const config = await desktopApi.getAppConfig();
      const codecs = recommendCodec(fromNativeCodecInfo(await desktopApi.getNativeSupportedCodecs()));
      const saved = await desktopApi.getStreamSettings();
      if (lifecycle.disposed) {
        return;
      }
      configEndpointRef.current = config.apiEndpoint;
      codecsRef.current = codecs;
      setLivekitUrl(config.livekitUrl);
      setAvailableCodecs(codecs);
      lastSavedSettingsRef.current = null;
      applySavedSettings(saved);
      setSettingsHydrated(true);
    })();
    return () => {
      lifecycle.disposed = true;
      void unlisten.then((stop) => {
        stop();
      });
    };
  }, [applySavedSettings]);

  useEffect(() => {
    if (!settingsHydrated) {
      return undefined;
    }
    const current: StreamSettings = {
      apiEndpoint,
      apiEndpointIsCustom,
      autoBitrate,
      bitrateLimit,
      fps: streamFps,
      motionMode,
      pendingApiEndpoint,
      resolution,
      videoCodec,
    };
    const last = lastSavedSettingsRef.current;
    if (last && streamSettingsEqual(last, current)) {
      return undefined;
    }

    const timer = setTimeout(() => {
      const changes: Partial<StreamSettings> = { ...current };
      const keys = [
        'fps',
        'bitrateLimit',
        'videoCodec',
        'resolution',
        'apiEndpoint',
        'apiEndpointIsCustom',
        'pendingApiEndpoint',
        'autoBitrate',
        'motionMode',
      ] as const;
      if (last) {
        for (const key of keys) {
          if (current[key] === last[key]) {
            delete changes[key];
          }
        }
      }
      void desktopApi
        .saveStreamSettings(changes)
        .then((ok) => {
          if (!ok) {
            notify('error', 'Settings save failed', 'Changes could not be saved or shared with the app.');
            return;
          }
          notify('success', 'Stream settings saved');
        })
        .catch((cause: unknown) => {
          notify('error', 'Settings save failed', String(cause));
        });
    }, SETTINGS_SAVE_DEBOUNCE_MS);
    return () => {
      clearTimeout(timer);
    };
  }, [
    settingsHydrated,
    streamFps,
    bitrateLimit,
    videoCodec,
    resolution,
    apiEndpoint,
    apiEndpointIsCustom,
    pendingApiEndpoint,
    autoBitrate,
    motionMode,
  ]);

  const activateApiEndpoint = useCallback((endpoint: string): void => {
    setApiEndpoint(endpoint);
    setApiEndpointIsCustom(true);
    setPendingApiEndpoint(null);
  }, []);

  return {
    activateApiEndpoint,
    apiEndpoint,
    autoBitrate,
    autoBitrateRef,
    availableCodecs,
    bitrateLimit,
    bitrateLimitRef,
    livekitUrl,
    motionMode,
    pendingApiEndpoint,
    resolution,
    resolutionRef,
    setAutoBitrate,
    setAvailableCodecs,
    setBitrateLimit,
    setLivekitUrl,
    setMotionMode,
    setPendingApiEndpoint,
    setResolution,
    setStreamFps,
    setVideoCodec,
    settingsHydrated,
    streamFps,
    streamFpsRef,
    videoCodec,
  };
};
