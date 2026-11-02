import { Settings } from 'lucide-react';
import type React from 'react';
import { useEffect, useState } from 'react';

import { desktopApi } from '@/api/desktop';
import { windowControls } from '@/api/windowControls';
import type { WindowButtonIcons } from '@/types';

type ButtonStyle = 'windows' | 'kde' | 'gnome';
type ButtonKind = keyof WindowButtonIcons;

// Used only when the icon theme provides no window icons.
const FALLBACK_PATHS = {
  close: 'M3.5 3.5 12.5 12.5M12.5 3.5 3.5 12.5',
  maximize: 'M3.5 3.5H12.5V12.5H3.5Z',
  minimize: 'M3 8.5H13',
  restore: 'M5.5 3.5H12.5V10.5M3.5 5.5H10.5V12.5H3.5Z',
} satisfies Record<ButtonKind, string>;

const LAYOUTS = {
  gnome: {
    button: 'size-6 rounded-full bg-foreground/10 text-foreground hover:bg-foreground/20',
    group: 'items-center gap-2 pl-1 pr-3',
  },
  kde: {
    button: 'size-6 rounded-full text-muted-foreground hover:bg-foreground/15 hover:text-foreground',
    group: 'items-center gap-1 pl-1 pr-2.5',
  },
  windows: {
    button: 'w-11 text-muted-foreground hover:bg-accent/60 hover:text-foreground',
    group: 'items-stretch',
  },
} satisfies Record<ButtonStyle, { group: string; button: string }>;

const CLOSE_HOVER = 'hover:bg-destructive hover:text-destructive-foreground';

const pickButtonStyle = (platform: string, desktopEnvironment: string): ButtonStyle => {
  if (platform !== 'linux') {
    return 'windows';
  }
  if (desktopEnvironment.includes('kde')) {
    return 'kde';
  }
  return 'gnome';
};

// Icon themes draw these at 16px with the glyph centred in the box, so the box is centred in the
// Button. The SVG is a mask so it takes the button's colour whatever colours the theme baked in.
const ThemeGlyph: React.FC<{ kind: ButtonKind; svg: string | null }> = ({ kind, svg }) => {
  if (svg === null || svg === '') {
    return (
      <svg
        viewBox="0 0 16 16"
        className="size-4"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.25"
        aria-hidden="true"
      >
        <path d={FALLBACK_PATHS[kind]} strokeLinecap="round" strokeLinejoin="round" />
      </svg>
    );
  }

  const mask = `url("data:image/svg+xml;utf8,${encodeURIComponent(svg)}")`;
  return (
    <span
      aria-hidden="true"
      className="size-4 bg-current"
      style={{ maskImage: mask, maskPosition: 'center', maskRepeat: 'no-repeat', maskSize: '16px 16px' }}
    />
  );
};

interface WindowButtonsProps {
  maximized: boolean;
  onSettings?: (() => void) | undefined;
}

export const WindowButtons: React.FC<WindowButtonsProps> = ({ maximized, onSettings }) => {
  const [style, setStyle] = useState<ButtonStyle>('windows');
  const [icons, setIcons] = useState<WindowButtonIcons | null>(null);
  const { group, button } = LAYOUTS[style];
  const toggleKind = maximized ? 'restore' : 'maximize';

  useEffect(() => {
    void desktopApi.getPlatformInfo().then((info) => {
      setStyle(pickButtonStyle(info.platform, info.desktopEnvironment));
    });
    void desktopApi.getWindowButtonIcons().then(setIcons);
  }, []);

  return (
    <div className="ml-auto flex items-stretch">
      {onSettings && (
        <button
          type="button"
          aria-label="Settings"
          title="Settings (Ctrl/Cmd+,)"
          onClick={onSettings}
          className="w-11 flex items-center justify-center text-muted-foreground transition-colors hover:bg-accent/60 hover:text-foreground focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
        >
          <Settings className="w-4 h-4" aria-hidden="true" />
        </button>
      )}
      <div className={`flex ${group}`}>
        <button
          type="button"
          aria-label="Minimize"
          onClick={() => void windowControls.minimize()}
          className={`flex items-center justify-center transition-colors ${button}`}
        >
          <ThemeGlyph kind="minimize" svg={icons?.minimize ?? null} />
        </button>
        <button
          type="button"
          aria-label={maximized ? 'Restore' : 'Maximize'}
          onClick={() => void windowControls.toggleMaximize()}
          className={`flex items-center justify-center transition-colors ${button}`}
        >
          <ThemeGlyph kind={toggleKind} svg={icons?.[toggleKind] ?? null} />
        </button>
        <button
          type="button"
          aria-label="Close"
          onClick={() => void windowControls.close()}
          className={`flex items-center justify-center transition-colors ${button} ${CLOSE_HOVER}`}
        >
          <ThemeGlyph kind="close" svg={icons?.close ?? null} />
        </button>
      </div>
    </div>
  );
};
