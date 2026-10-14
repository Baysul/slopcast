import type { AudioApp, AudioAppWave, StreamSettings } from '@slopcast/shared-types';
import { DEFAULT_STREAM_SETTINGS } from '@slopcast/shared-types';
import { invoke } from '@tauri-apps/api/core';
import { emitTo, listen, type UnlistenFn } from '@tauri-apps/api/event';
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

import type {
  ApiEndpointEdit,
  AppearanceMode,
  ColorTheme,
  ControlState,
  DesktopAction,
  DesktopPreferences,
  PreferencesSnapshot,
  ShortcutBindings,
  ShortcutReport,
  TypographyPreferences,
} from '../types/preferences';

import type { ResizeDirection } from './windowControls';

type TauriInvokeArguments = Parameters<typeof invoke>[1];

const unavailableCommands = new Set<string>();

const warnUnavailable = (cmd: string, cause: unknown): void => {
  if (unavailableCommands.has(cmd)) return;
  unavailableCommands.add(cmd);
  console.warn(`[desktop] command "${cmd}" unavailable, using fallback:`, cause);
};

async function invokeOr<T>(cmd: string, args: TauriInvokeArguments, fallback: T): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (err) {
    warnUnavailable(cmd, err);
    return fallback;
  }
}

async function invokeOk(cmd: string, args: TauriInvokeArguments): Promise<boolean> {
  try {
    await invoke(cmd, args);
    return true;
  } catch (err) {
    warnUnavailable(cmd, err);
    return false;
  }
}

async function invokeErr(cmd: string, args: TauriInvokeArguments): Promise<string | null> {
  try {
    await invoke(cmd, args);
    return null;
  } catch (cause) {
    warnUnavailable(cmd, cause);
    if (Object.prototype.toString.call(cause) === '[object String]') {
      const message = String(cause);
      if (message.length > 0) return message;
    }
    return `Command ${cmd} unavailable`;
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
  getDesktopPreferences: (): Promise<PreferencesSnapshot> => invoke('get_desktop_preferences'),
  saveGeneralPreferences: (trayEnabled: boolean, autoSelectAudio: boolean): Promise<PreferencesSnapshot> =>
    invoke('save_general_preferences', { trayEnabled, autoSelectAudio }),
  saveAppearancePreferences: (appearanceMode: AppearanceMode, colorTheme: ColorTheme): Promise<PreferencesSnapshot> =>
    invoke('save_appearance_preferences', { appearanceMode, colorTheme }),
  saveTypographyPreferences: (typography: TypographyPreferences): Promise<PreferencesSnapshot> =>
    invoke('save_typography_preferences', { typography }),
  chooseSettingsDirectory: (): Promise<PreferencesSnapshot> => invoke('choose_settings_directory'),
  openSettings: (): Promise<void> => invoke('open_settings'),
  beginWindowDrag: (kind: ResizeDirection | 'Move'): Promise<void> => invoke('begin_window_drag', { kind }),
  showMainWindow: (): Promise<void> => invoke('show_main_window'),
  getControlState: (): Promise<ControlState> => invoke('get_control_state'),
  saveTelemetryShortcut: (chord: string | null): Promise<PreferencesSnapshot> =>
    invoke('save_telemetry_shortcut', { chord }),
  editApiEndpoint: (edit: ApiEndpointEdit): Promise<void> => emitTo('main', 'api-endpoint-edited', edit),
  onApiEndpointEdit: (callback: (edit: ApiEndpointEdit) => void): Promise<UnlistenFn> =>
    subscribe('api-endpoint-edited', callback),
  onControlStateChanged: (callback: (state: ControlState) => void): Promise<UnlistenFn> =>
    subscribe('desktop-control-state-changed', callback),
  onStreamSettingsChanged: (callback: (settings: StreamSettings) => void): Promise<UnlistenFn> =>
    subscribe('stream-settings-changed', callback),
  quitApp: (): Promise<void> => invoke('quit_app'),
  updateControlState: (state: ControlState): Promise<void> => invoke('update_control_state', { state }),
  getShortcutStatus: (): Promise<ShortcutReport> => invoke('get_shortcut_status'),
  saveShortcuts: (bindings: ShortcutBindings): Promise<ShortcutReport> => invoke('save_shortcuts', { bindings }),
  configurePortalShortcuts: (): Promise<void> => invoke('configure_portal_shortcuts'),
  setShortcutRecording: (recording: boolean): Promise<void> => invoke('set_shortcut_recording', { recording }),
  getAudioMuted: (): Promise<boolean> => invoke('get_audio_muted'),
  toggleAudioMute: (): Promise<boolean> => invoke('toggle_audio_mute'),
  onDesktopAction: (callback: (action: DesktopAction) => void): Promise<UnlistenFn> =>
    subscribe('desktop-action', callback),
  onDesktopPreferences: (callback: (preferences: DesktopPreferences) => void): Promise<UnlistenFn> =>
    subscribe('desktop-preferences-changed', callback),
  onShortcutsChanged: (callback: (report: ShortcutReport) => void): Promise<UnlistenFn> =>
    subscribe('shortcuts-changed', callback),
  onSettingsDirectoryChanged: (callback: () => void): Promise<UnlistenFn> =>
    subscribe('settings-directory-changed', callback),
  onSettingsCloseRequested: (callback: () => void): Promise<UnlistenFn> =>
    subscribe('tauri://close-requested', callback),
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
  saveStreamSettings: (settings: Partial<StreamSettings>): Promise<boolean> =>
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
      cursorFrames: 0,
      cursorMissing: false,
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
