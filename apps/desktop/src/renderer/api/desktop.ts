// Typed Tauri command wrapper — the renderer's only backend entry point.
// The surface is exactly the MIGRATION.md §5 table: every preload channel maps
// to a snake_case command (camelCase args), audio waves and preview frames come
// in as events. Each call degrades gracefully: when the command is missing or
// the backend isn't merged yet, it resolves to a typed fallback (false / null /
// default) and warns once per command, so the renderer never crashes on a
// rejected invoke.

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

// Value-returning commands: rejections (missing command, backend error) map to
// the caller's fallback.
async function invokeOr<T>(cmd: string, args: Record<string, unknown> | undefined, fallback: T): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (err) {
    warnUnavailable(cmd, err);
    return fallback;
  }
}

// Unit-returning commands (`Result<(), String>`): resolved means success,
// rejected means failure (or a missing command).
async function invokeOk(cmd: string, args: Record<string, unknown> | undefined): Promise<boolean> {
  try {
    await invoke(cmd, args);
    return true;
  } catch (err) {
    warnUnavailable(cmd, err);
    return false;
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
  // `sourceId` is dropped (D2): the Wayland cascade introspects, name-matches
  // and falls back to the capture context on its own.
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
  connectNativeRoom: (url: string, token: string): Promise<boolean> =>
    invokeOk('connect_native_room', { args: { url, token } }),
  disconnectNativeRoom: (): Promise<boolean> => invokeOk('disconnect_native_room', undefined),
  isNativeRoomConnected: (): Promise<boolean> => invokeOr('is_native_room_connected', undefined, false),
  startNativeCapture: (config: DesktopCaptureConfig, source?: CaptureSourceSelection): Promise<CaptureStartResult> =>
    invokeOr('start_native_capture', source ? { config, source } : { config }, {
      ok: false,
      nodeId: null,
      videoEnabled: false,
    }),
  // Headless test-pattern capture (e2e): synthetic BGRA frames feed the exact
  // same publish path as the portal capture.
  startSyntheticCapture: (config: DesktopCaptureConfig): Promise<CaptureStartResult> =>
    invokeOr('start_synthetic_capture', { config }, { ok: false, nodeId: null, videoEnabled: false }),
  getNativeSupportedCodecs: (): Promise<NativeCodecInfo[]> => invokeOr('get_native_supported_codecs', undefined, []),
  updateNativeVideo: (config: DesktopCaptureConfig): Promise<boolean> =>
    invokeOr('update_native_video', { config }, false),
  stopNativeCapture: (): Promise<boolean> => invokeOr('stop_native_capture', undefined, false),
  stopVideoCapture: (): Promise<boolean> => invokeOr('stop_video_capture', undefined, false),
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
      lastWidth: 0,
      lastHeight: 0,
    }),
  // Pre-roll flow (§9): capture-only mode and publish-with-audio. On
  // Windows, `source` is the picker's selection (required for real capture);
  // on Linux it is ignored — the portal picker decides.
  startCapturePreview: (source?: CaptureSourceSelection): Promise<boolean> =>
    invokeOk('start_capture_preview', source ? { source } : undefined),
  goLive: (config: DesktopCaptureConfig, source?: CaptureSourceSelection): Promise<boolean> =>
    invokeOk('go_live', source ? { config, source } : { config }),
  // Windows WGC source enumeration for the in-app picker (empty on other
  // platforms, where the portal picker or the platform gate applies).
  getCaptureSources: (): Promise<CaptureSourceInfo[]> => invokeOr('get_capture_sources', undefined, []),
  probeGpuInfo: (): Promise<GpuInfo | null> => invokeOr('probe_gpu_info', undefined, null),
  // Preview transport: the backend keeps the latest raw BGRA frame and
  // serves it directly via a `frame://` custom protocol (no tauri IPC
  // needed — tauri's raw-body delivery is too slow on WebKitGTK). The
  // renderer fetches it via `fetch('frame://...')` at its own pace.
  getPreviewFrame: (): Promise<ArrayBuffer> => fetch(`frame://frame.bin?t=${Date.now()}`).then((r) => r.arrayBuffer()),
  // The renderer reports its preview card size (device pixels) so the backend
  // scales preview frames to fit the card (OBS-style) instead of shipping
  // full-resolution JPEGs through the channel.
  setPreviewViewport: (width: number, height: number): Promise<boolean> =>
    invokeOk('set_preview_viewport', { width, height }),
  clearPreviewViewport: (): Promise<boolean> => invokeOk('clear_preview_viewport', undefined),
  onAudioWave: (callback: (waves: AudioAppWave[]) => void): Promise<UnlistenFn> =>
    subscribe<AudioAppWave[]>('audio-wave-update', callback),
  // Fired when the portal closes the ScreenCast session — the presenter
  // closed the captured window/app, so the backend ended the share. The
  // renderer tears the UI down (same path as the Stop button).
  onCaptureEnded: (callback: () => void): Promise<UnlistenFn> => subscribe<null>('capture-ended', () => callback()),
};
