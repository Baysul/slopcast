import type { MotionMode, ResolutionPreset, StreamSettings, VideoCodec } from '@slopcast/shared-types';
import { DEFAULT_STREAM_SETTINGS } from '@slopcast/shared-types';
import { useCallback, useEffect, useRef, useState } from 'react';
import { desktopApi } from '../api/desktop';
import { notify } from '../lib/toast';
import type { CodecInfo } from '../utils/codecs';
import { fromNativeCodecInfo, recommendCodec } from '../utils/codecs';

const SETTINGS_SAVE_DEBOUNCE_MS = 800;

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
  streamSettingsOpen: boolean;
  setStreamSettingsOpen: React.Dispatch<React.SetStateAction<boolean>>;
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

export function useStreamSettings(): UseStreamSettingsReturn {
  const [apiEndpoint, setApiEndpoint] = useState<string>('http://localhost:3001');
  const [apiEndpointIsCustom, setApiEndpointIsCustom] = useState(false);
  const [pendingApiEndpoint, setPendingApiEndpoint] = useState<string | null>(null);
  const [livekitUrl, setLivekitUrl] = useState<string>('');
  const [streamSettingsOpen, setStreamSettingsOpen] = useState(false);
  const [streamFps, setStreamFps] = useState(DEFAULT_STREAM_SETTINGS.fps);
  const [bitrateLimit, setBitrateLimit] = useState(DEFAULT_STREAM_SETTINGS.bitrateLimit);
  const [availableCodecs, setAvailableCodecs] = useState<CodecInfo[]>([]);
  const [videoCodec, setVideoCodec] = useState<VideoCodec>(DEFAULT_STREAM_SETTINGS.videoCodec);
  const [resolution, setResolution] = useState<ResolutionPreset>(DEFAULT_STREAM_SETTINGS.resolution);
  const [autoBitrate, setAutoBitrate] = useState<boolean>(DEFAULT_STREAM_SETTINGS.autoBitrate);
  const [motionMode, setMotionMode] = useState<MotionMode>(DEFAULT_STREAM_SETTINGS.motionMode);

  const streamFpsRef = useRef(streamFps);
  const bitrateLimitRef = useRef(bitrateLimit);
  const resolutionRef = useRef(resolution);
  const autoBitrateRef = useRef(autoBitrate);

  const settingsHydratedRef = useRef(false);
  const [settingsHydrated, setSettingsHydrated] = useState(false);
  const lastSavedSettingsRef = useRef<StreamSettings | null>(null);

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
    (async () => {
      const config = await desktopApi.getAppConfig();
      setLivekitUrl(config.livekitUrl);

      const codecs = recommendCodec(fromNativeCodecInfo(await desktopApi.getNativeSupportedCodecs()));
      setAvailableCodecs(codecs);

      const saved = await desktopApi.getStreamSettings();
      const savedOk = codecs.some((c) => c.codec === saved.videoCodec);
      const bestCodec = codecs[0] ? codecs[0].codec : 'vp8';
      const hydratedCodec = savedOk ? saved.videoCodec : bestCodec;
      const hydratedEndpoint = saved.apiEndpointIsCustom ? saved.apiEndpoint : config.apiEndpoint || saved.apiEndpoint;
      const hydratedSettings = { ...saved, apiEndpoint: hydratedEndpoint, videoCodec: hydratedCodec };
      lastSavedSettingsRef.current = hydratedSettings;
      setStreamFps(saved.fps);
      setBitrateLimit(saved.bitrateLimit);
      setVideoCodec(hydratedCodec);
      setResolution(saved.resolution);
      setApiEndpoint(hydratedEndpoint);
      setApiEndpointIsCustom(saved.apiEndpointIsCustom);
      setPendingApiEndpoint(saved.pendingApiEndpoint);
      setAutoBitrate(saved.autoBitrate);
      setMotionMode(saved.motionMode);
      settingsHydratedRef.current = true;
      setSettingsHydrated(true);
    })();
  }, []);

  useEffect(() => {
    if (!settingsHydratedRef.current) return;
    const current: StreamSettings = {
      fps: streamFps,
      bitrateLimit,
      videoCodec,
      resolution,
      apiEndpoint,
      apiEndpointIsCustom,
      pendingApiEndpoint,
      autoBitrate,
      motionMode,
    };
    const last = lastSavedSettingsRef.current;
    if (last && streamSettingsEqual(last, current)) return;

    const timer = setTimeout(() => {
      void desktopApi
        .saveStreamSettings(current)
        .then((ok) => {
          if (!ok) {
            notify('error', 'Settings save failed', 'The settings file could not be written to disk.');
            return;
          }
          lastSavedSettingsRef.current = current;
          notify('success', 'Stream settings saved');
        })
        .catch((cause: unknown) => {
          console.error('[useStreamSettings] saveStreamSettings IPC failed:', cause);
        });
    }, SETTINGS_SAVE_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [
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
    apiEndpoint,
    activateApiEndpoint,
    pendingApiEndpoint,
    setPendingApiEndpoint,
    livekitUrl,
    setLivekitUrl,
    streamSettingsOpen,
    setStreamSettingsOpen,
    streamFps,
    setStreamFps,
    bitrateLimit,
    setBitrateLimit,
    availableCodecs,
    setAvailableCodecs,
    videoCodec,
    setVideoCodec,
    resolution,
    setResolution,
    autoBitrate,
    setAutoBitrate,
    motionMode,
    setMotionMode,
    streamFpsRef,
    bitrateLimitRef,
    resolutionRef,
    autoBitrateRef,
    settingsHydrated,
  };
}
