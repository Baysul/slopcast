import type { AudioApp, AudioAppWave, StreamSettings } from '@slopcast/shared-types';
import { DEFAULT_STREAM_SETTINGS } from '@slopcast/shared-types';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type {
  AppConfig,
  CaptureContext,
  CaptureSourceInfo,
  CaptureSourceSelection,
  CaptureStartResult,
  DesktopCaptureConfig,
  DesktopCaptureStats,
  GpuInfo,
  NativeCodecInfo,
  NativeTelemetry,
  PlatformInfo,
} from '../types';

const unavailableCommands = new Set<string>();

const warnUnavailable = (cmd: string, err: unknown): void => {
  if (unavailableCommands.has(cmd)) return;
  unavailableCommands.add(cmd);
  console.warn(`[desktop] command "${cmd}" unavailable, using fallback:`, err);
};

async function invokeOr<T>(cmd: string, args: Record<string, unknown> | undefined, fallback: T): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (err) {
    warnUnavailable(cmd, err);
    return fallback;
  }
}

async function invokeOk(cmd: string, args: Record<string, unknown> | undefined): Promise<boolean> {
  try {
    await invoke(cmd, args);
    return true;
  } catch (err) {
    warnUnavailable(cmd, err);
    return false;
  }
}

async function invokeErr(cmd: string, args: Record<string, unknown> | undefined): Promise<string | null> {
  try {
    await invoke(cmd, args);
    return null;
  } catch (err) {
    warnUnavailable(cmd, err);
    return typeof err === 'string' && err.length > 0 ? err : `Command ${cmd} unavailable`;
  }
}

async function subscribe<T>(event: string, callback: (payload: T) => void): Promise<UnlistenFn> {
  try {
    return await listen<T>(event, (e) => callback(e.payload));
  } catch (err) {
    console.warn(`[desktop] event "${event}" unavailable:`, err);
    return () => undefined;
  }
}

export const desktopApi = {
  getAppConfig: (): Promise<AppConfig> => invokeOr('get_app_config', undefined, { apiEndpoint: '', livekitUrl: '' }),
  getPlatformInfo: (): Promise<PlatformInfo> =>
    invokeOr('get_platform_info', undefined, {
      platform: 'unknown',
      isWayland: false,
      videoCaptureAvailable: false,
    }),
  getAudioApps: (): Promise<AudioApp[]> => invokeOr('get_audio_apps', undefined, []),
  dumpAudioSources: (): Promise<Array<Record<string, string>>> => invokeOr('dump_audio_sources', undefined, []),
  startAudioCapture: (targetId: number): Promise<boolean> => invokeOr('start_audio_capture', { targetId }, false),
  stopAudioCapture: (): Promise<boolean> => invokeOr('stop_audio_capture', undefined, false),
  switchAudioCapture: (targetId: number): Promise<boolean> => invokeOr('switch_audio_capture', { targetId }, false),
  startAudioMetering: (): Promise<boolean> => invokeOr('start_audio_metering', undefined, false),
  stopAudioMetering: (): Promise<boolean> => invokeOr('stop_audio_metering', undefined, false),
  resolveAudioSource: (nameHint?: string): Promise<AudioApp | null> =>
    invokeOr('resolve_audio_source', nameHint ? { nameHint } : undefined, null),
  getCaptureContext: (): Promise<CaptureContext | null> => invokeOr('get_capture_context', undefined, null),
  inspectCaptureContext: (): Promise<CaptureContext | null> => invokeOr('inspect_capture_context', undefined, null),
  getStreamSettings: (): Promise<StreamSettings> =>
    invokeOr('get_stream_settings', undefined, { ...DEFAULT_STREAM_SETTINGS }),
  saveStreamSettings: (settings: StreamSettings): Promise<boolean> =>
    invokeOr('save_stream_settings', { settings }, false),
  getOnboardingCompleted: (): Promise<boolean> => invokeOr('get_onboarding_completed', undefined, false),
  setOnboardingCompleted: (): Promise<boolean> => invokeOr('set_onboarding_completed', undefined, false),
  connectNativeRoom: (url: string, token: string, roomName: string, identity: string): Promise<string | null> =>
    invokeErr('connect_native_room', { args: { url, token, roomName, identity } }),
  disconnectNativeRoom: (): Promise<boolean> => invokeOk('disconnect_native_room', undefined),
  isNativeRoomConnected: (): Promise<boolean> => invokeOr('is_native_room_connected', undefined, false),
  hasNativeRoomSession: (): Promise<boolean> => invokeOr('has_native_room_session', undefined, false),
  startNativeCapture: (config: DesktopCaptureConfig, source?: CaptureSourceSelection): Promise<CaptureStartResult> =>
    invokeOr('start_native_capture', source ? { config, source } : { config }, {
      ok: false,
      nodeId: null,
      videoEnabled: false,
    }),
  startSyntheticCapture: (config: DesktopCaptureConfig): Promise<CaptureStartResult> =>
    invokeOr('start_synthetic_capture', { config }, { ok: false, nodeId: null, videoEnabled: false }),
  getNativeSupportedCodecs: (): Promise<NativeCodecInfo[]> => invokeOr('get_native_supported_codecs', undefined, []),
  updateNativeVideo: (config: DesktopCaptureConfig): Promise<boolean> =>
    invokeOr('update_native_video', { config }, false),
  stopNativeCapture: (): Promise<boolean> => invokeOk('stop_native_capture', undefined),
  stopVideoCapture: (): Promise<boolean> => invokeOk('stop_video_capture', undefined),
  isNativeCaptureActive: (): Promise<boolean> => invokeOr('is_native_capture_active', undefined, false),
  getSpectatorCount: (): Promise<number> => invokeOr('get_spectator_count', undefined, 0),
  getNativeTelemetry: (): Promise<NativeTelemetry | null> => invokeOr('get_native_telemetry', undefined, null),
  getVideoCaptureStats: (): Promise<DesktopCaptureStats> =>
    invokeOr('get_video_capture_stats', undefined, {
      framesDequeued: 0,
      framesPushed: 0,
      framesDropped: 0,
      captureErrors: 0,
      previewFramesSent: 0,
      keepaliveAttempted: 0,
      keepalivePushed: 0,
      keepaliveDropped: 0,
      lastWidth: 0,
      lastHeight: 0,
      pacerPushes: 0,
      pacerPops: 0,
      pacerDrops: 0,
      pacerDepth: 0,
      pacerMaxDepth: 0,
    }),
  startCapturePreview: (source?: CaptureSourceSelection): Promise<boolean> =>
    invokeOk('start_capture_preview', source ? { source } : undefined),
  goLive: (config: DesktopCaptureConfig, source?: CaptureSourceSelection): Promise<boolean> =>
    invokeOk('go_live', source ? { config, source } : { config }),
  getCaptureSources: (): Promise<CaptureSourceInfo[]> => invokeOr('get_capture_sources', undefined, []),
  probeGpuInfo: (): Promise<GpuInfo | null> => invokeOr('probe_gpu_info', undefined, null),
  getPreviewFrame: (): Promise<ArrayBuffer> =>
    fetch(`http://frame.localhost/frame.bin?t=${Date.now()}`).then((r) => r.arrayBuffer()),
  setPreviewViewport: (width: number, height: number): Promise<boolean> =>
    invokeOk('set_preview_viewport', { width, height }),
  clearPreviewViewport: (): Promise<boolean> => invokeOk('clear_preview_viewport', undefined),
  onAudioWave: (callback: (waves: AudioAppWave[]) => void): Promise<UnlistenFn> =>
    subscribe<AudioAppWave[]>('audio-wave-update', callback),
  onCaptureEnded: (callback: () => void): Promise<UnlistenFn> => subscribe<null>('capture-ended', () => callback()),
};
