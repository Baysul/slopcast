import { DEFAULT_STREAM_SETTINGS } from '@slopcast/shared-types';
import type { AudioApp, AudioAppWave, StreamSettings } from '@slopcast/shared-types';
import { invoke } from '@tauri-apps/api/core';
import { emitTo, listen } from '@tauri-apps/api/event';
import type { UnlistenFn } from '@tauri-apps/api/event';

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
  WindowButtonIcons,
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
  if (unavailableCommands.has(cmd)) {
    return;
  }
  unavailableCommands.add(cmd);
  console.warn(`[desktop] command "${cmd}" unavailable, using fallback:`, cause);
};

const invokeOr = async <T>(cmd: string, args: TauriInvokeArguments, fallback: T): Promise<T> => {
  try {
    return await invoke<T>(cmd, args);
  } catch (err) {
    warnUnavailable(cmd, err);
    return fallback;
  }
};

const invokeOk = async (cmd: string, args: TauriInvokeArguments): Promise<boolean> => {
  try {
    await invoke(cmd, args);
    return true;
  } catch (err) {
    warnUnavailable(cmd, err);
    return false;
  }
};

const invokeErr = async (cmd: string, args: TauriInvokeArguments): Promise<string | null> => {
  try {
    await invoke(cmd, args);
    return null;
  } catch (cause) {
    warnUnavailable(cmd, cause);
    if (Object.prototype.toString.call(cause) === '[object String]') {
      const message = String(cause);
      if (message.length > 0) {
        return message;
      }
    }
    return `Command ${cmd} unavailable`;
  }
};

interface DesktopEventMap {
  'api-endpoint-edited': ApiEndpointEdit;
  'audio-wave-update': AudioAppWave[];
  'capture-ended': null;
  'desktop-control-state-changed': ControlState;
  'desktop-action': DesktopAction;
  'desktop-preferences-changed': DesktopPreferences;
  'settings-directory-changed': null;
  'shortcuts-changed': ShortcutReport;
  'stream-settings-changed': StreamSettings;
  'tauri://close-requested': null;
}

const subscribe = async <Name extends keyof DesktopEventMap>(
  event: Name,
  callback: (payload: DesktopEventMap[Name]) => void,
): Promise<UnlistenFn> => {
  try {
    return await listen<DesktopEventMap[Name]>(event, (e) => {
      callback(e.payload);
    });
  } catch (err) {
    console.warn(`[desktop] event "${event}" unavailable:`, err);
    return () => undefined;
  }
};

export const desktopApi = {
  beginWindowDrag: (kind: ResizeDirection | 'Move'): Promise<void> => invoke('begin_window_drag', { kind }),
  chooseSettingsDirectory: (): Promise<PreferencesSnapshot> => invoke('choose_settings_directory'),
  clearPreviewViewport: (): Promise<boolean> => invokeOk('clear_preview_viewport', undefined),
  configurePortalShortcuts: (): Promise<void> => invoke('configure_portal_shortcuts'),
  connectNativeRoom: (url: string, token: string, roomName: string, identity: string): Promise<string | null> =>
    invokeErr('connect_native_room', { args: { identity, roomName, token, url } }),
  disconnectNativeRoom: (): Promise<boolean> => invokeOk('disconnect_native_room', undefined),
  dumpAudioSources: (): Promise<Record<string, string>[]> => invokeOr('dump_audio_sources', undefined, []),
  editApiEndpoint: (edit: ApiEndpointEdit): Promise<void> => emitTo('main', 'api-endpoint-edited', edit),
  getAppConfig: (): Promise<AppConfig> => invokeOr('get_app_config', undefined, { apiEndpoint: '', livekitUrl: '' }),
  getAudioApps: (): Promise<AudioApp[]> => invokeOr('get_audio_apps', undefined, []),
  getAudioMuted: (): Promise<boolean> => invoke('get_audio_muted'),
  getCaptureContext: (): Promise<CaptureContext | null> => invokeOr('get_capture_context', undefined, null),
  getCaptureSources: (): Promise<CaptureSourceInfo[]> => invokeOr('get_capture_sources', undefined, []),
  getControlState: (): Promise<ControlState> => invoke('get_control_state'),
  getDesktopPreferences: (): Promise<PreferencesSnapshot> => invoke('get_desktop_preferences'),
  getNativeSupportedCodecs: (): Promise<NativeCodecInfo[]> => invokeOr('get_native_supported_codecs', undefined, []),
  getNativeTelemetry: (): Promise<NativeTelemetry | null> => invokeOr('get_native_telemetry', undefined, null),
  getOnboardingCompleted: (): Promise<boolean> => invokeOr('get_onboarding_completed', undefined, false),
  getPlatformInfo: (): Promise<PlatformInfo> =>
    invokeOr('get_platform_info', undefined, {
      desktopEnvironment: '',
      isWayland: false,
      platform: 'unknown',
      videoCaptureAvailable: false,
    }),
  getPreviewFrame: (): Promise<ArrayBuffer> =>
    fetch(`http://frame.localhost/frame.bin?t=${Date.now()}`).then((r) => r.arrayBuffer()),
  getShortcutStatus: (): Promise<ShortcutReport> => invoke('get_shortcut_status'),
  getSpectatorCount: (): Promise<number> => invokeOr('get_spectator_count', undefined, 0),
  getStreamSettings: (): Promise<StreamSettings> =>
    invokeOr('get_stream_settings', undefined, { ...DEFAULT_STREAM_SETTINGS }),
  getVideoCaptureStats: (): Promise<DesktopCaptureStats> =>
    invokeOr('get_video_capture_stats', undefined, {
      captureErrors: 0,
      cursorFrames: 0,
      cursorMissing: false,
      framesDequeued: 0,
      framesDropped: 0,
      framesPushed: 0,
      keepaliveAttempted: 0,
      keepaliveDropped: 0,
      keepalivePushed: 0,
      lastHeight: 0,
      lastWidth: 0,
      pacerDepth: 0,
      pacerDrops: 0,
      pacerMaxDepth: 0,
      pacerPops: 0,
      pacerPushes: 0,
      previewFramesSent: 0,
    }),
  getWindowButtonIcons: (): Promise<WindowButtonIcons> =>
    invokeOr('get_window_button_icons', undefined, { close: null, maximize: null, minimize: null, restore: null }),
  goLive: (config: DesktopCaptureConfig, source?: CaptureSourceSelection): Promise<boolean> =>
    invokeOk('go_live', source ? { config, source } : { config }),
  hasNativeRoomSession: (): Promise<boolean> => invokeOr('has_native_room_session', undefined, false),
  inspectCaptureContext: (): Promise<CaptureContext | null> => invokeOr('inspect_capture_context', undefined, null),
  isNativeCaptureActive: (): Promise<boolean> => invokeOr('is_native_capture_active', undefined, false),
  isNativeRoomConnected: (): Promise<boolean> => invokeOr('is_native_room_connected', undefined, false),
  onApiEndpointEdit: (callback: (edit: ApiEndpointEdit) => void): Promise<UnlistenFn> =>
    subscribe('api-endpoint-edited', callback),
  onAudioWave: (callback: (waves: AudioAppWave[]) => void): Promise<UnlistenFn> =>
    subscribe('audio-wave-update', callback),
  onCaptureEnded: (callback: () => void): Promise<UnlistenFn> =>
    subscribe('capture-ended', () => {
      callback();
    }),
  onControlStateChanged: (callback: (state: ControlState) => void): Promise<UnlistenFn> =>
    subscribe('desktop-control-state-changed', callback),
  onDesktopAction: (callback: (action: DesktopAction) => void): Promise<UnlistenFn> =>
    subscribe('desktop-action', callback),
  onDesktopPreferences: (callback: (preferences: DesktopPreferences) => void): Promise<UnlistenFn> =>
    subscribe('desktop-preferences-changed', callback),
  onSettingsCloseRequested: (callback: () => void): Promise<UnlistenFn> =>
    subscribe('tauri://close-requested', callback),
  onSettingsDirectoryChanged: (callback: () => void): Promise<UnlistenFn> =>
    subscribe('settings-directory-changed', callback),
  onShortcutsChanged: (callback: (report: ShortcutReport) => void): Promise<UnlistenFn> =>
    subscribe('shortcuts-changed', callback),
  onStreamSettingsChanged: (callback: (settings: StreamSettings) => void): Promise<UnlistenFn> =>
    subscribe('stream-settings-changed', callback),
  openSettings: (): Promise<void> => invoke('open_settings'),
  probeGpuInfo: (): Promise<GpuInfo | null> => invokeOr('probe_gpu_info', undefined, null),
  quitApp: (): Promise<void> => invoke('quit_app'),
  resolveAudioSource: (nameHint?: string): Promise<AudioApp | null> =>
    invokeOr('resolve_audio_source', nameHint !== undefined && nameHint !== '' ? { nameHint } : undefined, null),
  saveAppearancePreferences: (appearanceMode: AppearanceMode, colorTheme: ColorTheme): Promise<PreferencesSnapshot> =>
    invoke('save_appearance_preferences', { appearanceMode, colorTheme }),
  saveGeneralPreferences: (trayEnabled: boolean, autoSelectAudio: boolean): Promise<PreferencesSnapshot> =>
    invoke('save_general_preferences', { autoSelectAudio, trayEnabled }),
  saveShortcuts: (bindings: ShortcutBindings): Promise<ShortcutReport> => invoke('save_shortcuts', { bindings }),
  saveStreamSettings: (settings: Partial<StreamSettings>): Promise<boolean> =>
    invokeOr('save_stream_settings', { settings }, false),
  saveTelemetryShortcut: (chord: string | null): Promise<PreferencesSnapshot> =>
    invoke('save_telemetry_shortcut', { chord }),
  saveTypographyPreferences: (typography: TypographyPreferences): Promise<PreferencesSnapshot> =>
    invoke('save_typography_preferences', { typography }),
  setOnboardingCompleted: (): Promise<boolean> => invokeOr('set_onboarding_completed', undefined, false),
  setPreviewViewport: (width: number, height: number): Promise<boolean> =>
    invokeOk('set_preview_viewport', { height, width }),
  setSettingsDirectory: (path: string): Promise<PreferencesSnapshot> => invoke('set_settings_directory', { path }),
  setShortcutRecording: (recording: boolean): Promise<void> => invoke('set_shortcut_recording', { recording }),
  showMainWindow: (): Promise<void> => invoke('show_main_window'),
  startAudioCapture: (targetId: number): Promise<boolean> => invokeOr('start_audio_capture', { targetId }, false),
  startAudioMetering: (): Promise<boolean> => invokeOr('start_audio_metering', undefined, false),
  startCapturePreview: (source?: CaptureSourceSelection): Promise<boolean> =>
    invokeOk('start_capture_preview', source ? { source } : undefined),
  startNativeCapture: (config: DesktopCaptureConfig, source?: CaptureSourceSelection): Promise<CaptureStartResult> =>
    invokeOr('start_native_capture', source ? { config, source } : { config }, {
      nodeId: null,
      ok: false,
      videoEnabled: false,
    }),
  startSyntheticCapture: (config: DesktopCaptureConfig): Promise<CaptureStartResult> =>
    invokeOr('start_synthetic_capture', { config }, { nodeId: null, ok: false, videoEnabled: false }),
  stopAudioCapture: (): Promise<boolean> => invokeOr('stop_audio_capture', undefined, false),
  stopAudioMetering: (): Promise<boolean> => invokeOr('stop_audio_metering', undefined, false),
  stopNativeCapture: (): Promise<boolean> => invokeOk('stop_native_capture', undefined),
  stopVideoCapture: (): Promise<boolean> => invokeOk('stop_video_capture', undefined),
  switchAudioCapture: (targetId: number): Promise<boolean> => invokeOr('switch_audio_capture', { targetId }, false),
  toggleAudioMute: (): Promise<boolean> => invoke('toggle_audio_mute'),
  updateControlState: (state: ControlState): Promise<void> => invoke('update_control_state', { state }),
  updateNativeRoomToken: (token: string): Promise<string | null> => invokeErr('update_native_room_token', { token }),
  updateNativeVideo: (config: DesktopCaptureConfig): Promise<boolean> =>
    invokeOr('update_native_video', { config }, false),
};
