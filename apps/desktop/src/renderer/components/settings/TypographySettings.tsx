import { Button } from '@/components/ui/button';
import { Switch } from '@/components/ui/switch';
import type { TypographyPreferences } from '@/types/preferences';
import { DEFAULT_TYPOGRAPHY } from '@/types/preferences';

const FONT_CHOICES = [
  { id: 'theme', label: 'Theme default' },
  { id: 'system', label: 'System sans' },
  { id: 'inter', label: 'Inter · sans serif' },
  { id: 'lora', label: 'Lora · serif' },
  { id: 'jetbrains-mono', label: 'JetBrains Mono · monospace' },
] as const;

interface TypographySettingsProps {
  typography: TypographyPreferences;
  busy: boolean;
  onChange: (typography: TypographyPreferences) => void;
}

export function TypographySettings({ typography, busy, onChange }: TypographySettingsProps) {
  return (
    <fieldset disabled={busy} aria-describedby="typography-hint" className="min-w-0 space-y-4">
      <legend className="text-base font-semibold">Typography</legend>
      <p id="typography-hint" className="text-sm text-muted-foreground">
        Theme defaults pair a font with each palette. Your overrides stay when you switch themes.
      </p>
      <div className="grid gap-4 sm:grid-cols-2">
        <div className="space-y-1.5 sm:col-span-2">
          <label htmlFor="appearance-font-family" className="block text-sm font-medium">
            Font family
          </label>
          <select
            id="appearance-font-family"
            className="w-full rounded-md border border-input bg-card px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
            value={typography.fontFamily}
            onChange={(event) => {
              const choice = FONT_CHOICES.find((font) => font.id === event.target.value);
              if (choice) onChange({ ...typography, fontFamily: choice.id });
            }}
          >
            {FONT_CHOICES.map(({ id, label }) => (
              <option key={id} value={id}>
                {label}
              </option>
            ))}
          </select>
        </div>
        <div className="space-y-1.5">
          <label htmlFor="appearance-font-size" className="block text-sm font-medium">
            Base text size
          </label>
          <select
            id="appearance-font-size"
            className="w-full rounded-md border border-input bg-card px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
            value={typography.fontSize ?? ''}
            onChange={(event) => {
              const fontSize = event.target.value === '' ? null : Number(event.target.value);
              onChange({ ...typography, fontSize });
            }}
          >
            <option value="">Theme default (14 px)</option>
            {[12, 13, 14, 15, 16, 17, 18].map((size) => (
              <option key={size} value={size}>
                {size} px
              </option>
            ))}
          </select>
        </div>
        <div className="space-y-1.5">
          <label htmlFor="appearance-font-weight" className="block text-sm font-medium">
            Body weight
          </label>
          <select
            id="appearance-font-weight"
            className="w-full rounded-md border border-input bg-card px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
            value={typography.fontWeight ?? ''}
            onChange={(event) => {
              const fontWeight = event.target.value === '' ? null : Number(event.target.value);
              onChange({ ...typography, fontWeight });
            }}
          >
            <option value="">Theme default (regular)</option>
            <option value="400">Regular</option>
            <option value="500">Medium</option>
            <option value="600">Semibold</option>
          </select>
        </div>
      </div>
      <div className="flex items-center justify-between gap-4">
        <div>
          <label id="ligatures-label" htmlFor="font-ligatures" className="text-sm font-medium">
            Font ligatures
          </label>
          <p id="ligatures-hint" className="text-xs text-muted-foreground">
            Combine supported letters and code symbols.
          </p>
        </div>
        <Switch
          id="font-ligatures"
          checked={typography.ligatures}
          disabled={busy}
          aria-labelledby="ligatures-label"
          aria-describedby="ligatures-hint"
          onCheckedChange={(ligatures) => onChange({ ...typography, ligatures })}
        />
      </div>
      <section className="border-y border-border py-4" aria-label="Typography preview">
        <p className="text-base font-semibold">A clear view, live.</p>
        <p className="text-sm text-body-text">Share your screen with a little flair. 0123456789</p>
        <p className="mt-1 text-sm font-mono text-muted-foreground">live =&gt; frames != 0</p>
      </section>
      <Button
        type="button"
        variant="outline"
        className="h-auto whitespace-normal"
        disabled={busy}
        onClick={() => onChange(DEFAULT_TYPOGRAPHY)}
      >
        Reset typography to theme defaults
      </Button>
      <p className="text-xs text-muted-foreground">
        Fonts are included in the app. Changes apply immediately—no restart or download.
      </p>
    </fieldset>
  );
}
