import { Monitor, Moon, Sun } from 'lucide-react';
import type { AppearanceMode, ColorTheme, DesktopPreferences, TypographyPreferences } from '@/types/preferences';
import { TypographySettings } from './TypographySettings';

const MODES = [
  { id: 'system', label: 'System', icon: Monitor },
  { id: 'light', label: 'Light', icon: Sun },
  { id: 'dark', label: 'Dark', icon: Moon },
] as const;

const THEMES = [
  { id: 'slopcast', label: 'Slopcast', variants: 'Default · warm bronze' },
  { id: 'ayu', label: 'Ayu', variants: 'Light / Dark · amber and slate' },
  { id: 'rose-pine', label: 'Rosé Pine', variants: 'Dawn / Pine · soft, muted tones' },
  { id: 'kanagawa', label: 'Kanagawa', variants: 'Lotus / Wave · ink and parchment' },
  { id: 'dracula', label: 'Dracula', variants: 'Alucard / Classic · purple accents' },
] as const;

interface AppearanceSettingsProps {
  preferences: DesktopPreferences;
  busy: boolean;
  onChange: (mode: AppearanceMode, theme: ColorTheme) => void;
  onTypographyChange: (typography: TypographyPreferences) => void;
}

export function AppearanceSettings({ preferences, busy, onChange, onTypographyChange }: AppearanceSettingsProps) {
  return (
    <div className="space-y-8">
      <fieldset disabled={busy} aria-describedby="appearance-mode-hint">
        <legend className="text-base font-semibold">Color mode</legend>
        <p id="appearance-mode-hint" className="mt-1 text-sm text-muted-foreground">
          System follows your device’s light or dark appearance.
        </p>
        <div className="mt-4 grid grid-cols-3 gap-2">
          {MODES.map(({ id, label, icon: Icon }) => (
            <label key={id} className="relative cursor-pointer">
              <input
                className="peer sr-only"
                type="radio"
                name="appearance-mode"
                value={id}
                checked={preferences.appearanceMode === id}
                onChange={() => onChange(id, preferences.colorTheme)}
              />
              <span className="flex items-center justify-center gap-2 rounded-md border border-border px-3 py-3 text-sm text-muted-foreground transition-colors hover:bg-secondary peer-checked:border-safelight peer-checked:bg-safelight/10 peer-checked:text-foreground peer-focus-visible:ring-2 peer-focus-visible:ring-ring peer-focus-visible:ring-offset-2 peer-focus-visible:ring-offset-background peer-disabled:cursor-not-allowed peer-disabled:opacity-50">
                <Icon className="size-4" aria-hidden="true" />
                {label}
              </span>
            </label>
          ))}
        </div>
      </fieldset>
      <fieldset disabled={busy} aria-describedby="appearance-theme-hint">
        <legend className="text-base font-semibold">Theme</legend>
        <p id="appearance-theme-hint" className="mt-1 text-sm text-muted-foreground">
          Each theme includes a light and dark palette. Previews show both.
        </p>
        <div className="mt-4 divide-y divide-border border-y border-border">
          {THEMES.map(({ id, label, variants }) => (
            <label
              key={id}
              className="flex cursor-pointer items-center gap-3 rounded-sm px-2 py-4 hover:bg-secondary/50 has-focus-visible:ring-2 has-focus-visible:ring-ring has-disabled:cursor-not-allowed has-disabled:opacity-50"
            >
              <input
                type="radio"
                name="color-theme"
                value={id}
                className="size-4 shrink-0 accent-primary"
                checked={preferences.colorTheme === id}
                onChange={() => onChange(preferences.appearanceMode, id)}
              />
              <span className="min-w-0 flex-1">
                <span className="block text-sm font-medium">{label}</span>
                <span className="block text-xs text-muted-foreground">{variants}</span>
              </span>
              <span className="flex shrink-0 gap-1.5" aria-hidden="true">
                {(['light', 'dark'] as const).map((mode) => (
                  <span
                    key={mode}
                    data-theme={id}
                    data-color-mode={mode}
                    className="flex gap-1 rounded-md border border-border bg-background p-1.5"
                  >
                    <span className="size-3 rounded-sm bg-foreground" />
                    <span className="size-3 rounded-sm bg-secondary" />
                    <span className="size-3 rounded-sm bg-safelight" />
                  </span>
                ))}
              </span>
            </label>
          ))}
        </div>
      </fieldset>
      <TypographySettings typography={preferences.typography} busy={busy} onChange={onTypographyChange} />
    </div>
  );
}
