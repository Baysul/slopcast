import type { CaptureStage } from './index';

export const SHORTCUT_ACTIONS = [
  { defaultChord: 'Control+Shift+F7', id: 'start-share', label: 'Start screenshare' },
  { defaultChord: 'Control+Shift+F8', id: 'stop-share', label: 'Stop screenshare' },
  { defaultChord: 'Control+Shift+F9', id: 'close-room', label: 'Close room' },
  { defaultChord: 'Control+Shift+F10', id: 'switch-source', label: 'Switch screenshare source' },
  { defaultChord: 'Control+Shift+F11', id: 'toggle-mute', label: 'Toggle audio mute' },
] as const;

export type ShortcutAction = (typeof SHORTCUT_ACTIONS)[number]['id'];
export type DesktopAction = ShortcutAction | 'quit';
export type ShortcutBindings = Readonly<Record<ShortcutAction, string>>;

export type AppearanceMode = 'system' | 'light' | 'dark';
export type ColorTheme = 'slopcast' | 'ayu' | 'rose-pine' | 'kanagawa' | 'dracula';

export type FontFamily = 'theme' | 'system' | 'inter' | 'lora' | 'jetbrains-mono';

export interface TypographyPreferences {
  readonly fontFamily: FontFamily;
  readonly fontSize: number | null;
  readonly fontWeight: number | null;
  readonly ligatures: boolean;
}

export const DEFAULT_TYPOGRAPHY: TypographyPreferences = {
  fontFamily: 'theme',
  fontSize: null,
  fontWeight: null,
  ligatures: true,
};

export interface DesktopPreferences {
  readonly trayEnabled: boolean;
  readonly autoSelectAudio: boolean;
  readonly appearanceMode: AppearanceMode;
  readonly colorTheme: ColorTheme;
  readonly typography: TypographyPreferences;
  readonly shortcuts: ShortcutBindings;
  readonly telemetryShortcut: string;
}

export interface PreferencesSnapshot {
  readonly preferences: DesktopPreferences;
  readonly settingsDirectory: string;
}

export interface ShortcutReport {
  readonly backend: string;
  readonly status: string;
  readonly bindings: Partial<ShortcutBindings>;
  readonly canConfigure: boolean;
}

export interface ControlState {
  readonly roomOpen: boolean;
  readonly captureStage: CaptureStage;
  readonly isMuted: boolean;
  readonly isBusy: boolean;
  readonly roomEndpoint: string | null;
  readonly endpointControlsDisabled: boolean;
  readonly endpointValidationRevision: number;
}

export type ApiEndpointEdit = { kind: 'validate'; endpoint: string } | { kind: 'reset' };

export const formatShortcut = (chord: string): string => {
  if (!chord) {
    return 'Not assigned';
  }
  return chord
    .replaceAll('Control', 'Ctrl')
    .replaceAll('Command', 'Cmd')
    .replaceAll('Key', '')
    .replaceAll('Digit', '')
    .replaceAll('+', ' + ');
};
