import type { CaptureStage } from './index';

export const SHORTCUT_ACTIONS = [
  { id: 'start-share', label: 'Start screenshare', defaultChord: 'Control+Shift+F7' },
  { id: 'stop-share', label: 'Stop screenshare', defaultChord: 'Control+Shift+F8' },
  { id: 'close-room', label: 'Close room', defaultChord: 'Control+Shift+F9' },
  { id: 'switch-source', label: 'Switch screenshare source', defaultChord: 'Control+Shift+F10' },
  { id: 'toggle-mute', label: 'Toggle audio mute', defaultChord: 'Control+Shift+F11' },
] as const;

export type ShortcutAction = (typeof SHORTCUT_ACTIONS)[number]['id'];
export type DesktopAction = ShortcutAction | 'quit';
export type ShortcutBindings = Record<ShortcutAction, string>;

export type AppearanceMode = 'system' | 'light' | 'dark';
export type ColorTheme = 'slopcast' | 'ayu' | 'rose-pine' | 'kanagawa' | 'dracula';

export type FontFamily = 'theme' | 'system' | 'inter' | 'lora' | 'jetbrains-mono';

export interface TypographyPreferences {
  fontFamily: FontFamily;
  fontSize: number | null;
  fontWeight: number | null;
  ligatures: boolean;
}

export const DEFAULT_TYPOGRAPHY: TypographyPreferences = {
  fontFamily: 'theme',
  fontSize: null,
  fontWeight: null,
  ligatures: true,
};

export interface DesktopPreferences {
  trayEnabled: boolean;
  autoSelectAudio: boolean;
  appearanceMode: AppearanceMode;
  colorTheme: ColorTheme;
  typography: TypographyPreferences;
  shortcuts: ShortcutBindings;
  telemetryShortcut: string;
}

export interface PreferencesSnapshot {
  preferences: DesktopPreferences;
  settingsDirectory: string;
}

export interface ShortcutReport {
  backend: string;
  status: string;
  bindings: Partial<ShortcutBindings>;
  canConfigure: boolean;
}

export interface ControlState {
  roomOpen: boolean;
  captureStage: CaptureStage;
  isMuted: boolean;
  isBusy: boolean;
  roomEndpoint: string | null;
  endpointControlsDisabled: boolean;
  endpointValidationRevision: number;
}

export type ApiEndpointEdit = { kind: 'validate'; endpoint: string } | { kind: 'reset' };

export function formatShortcut(chord: string): string {
  if (!chord) return 'Not assigned';
  return chord
    .replaceAll('Control', 'Ctrl')
    .replaceAll('Command', 'Cmd')
    .replaceAll('Key', '')
    .replaceAll('Digit', '')
    .replaceAll('+', ' + ');
}
