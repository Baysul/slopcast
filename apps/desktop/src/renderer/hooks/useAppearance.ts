import { useEffect, useLayoutEffect, useState } from 'react';
import type { DesktopPreferences } from '../types/preferences';

const FONT_FAMILIES = {
  system: 'var(--system-font-family)',
  inter: 'Inter, var(--system-font-family)',
  lora: 'Lora, Georgia, serif',
  'jetbrains-mono': '"JetBrains Mono", ui-monospace, monospace',
};

export function useAppearance(preferences: DesktopPreferences | undefined) {
  const [isSystemDark, setSystemDark] = useState(() => window.matchMedia('(prefers-color-scheme: dark)').matches);
  const mode = preferences?.appearanceMode ?? 'dark';
  const theme = preferences?.colorTheme ?? 'slopcast';
  const typography = preferences?.typography;
  let colorMode: 'light' | 'dark' = 'light';
  if (mode === 'dark' || (mode === 'system' && isSystemDark)) colorMode = 'dark';

  useEffect(() => {
    const media = window.matchMedia('(prefers-color-scheme: dark)');
    const update = () => setSystemDark(media.matches);
    update();
    media.addEventListener('change', update);
    return () => media.removeEventListener('change', update);
  }, []);
  useLayoutEffect(() => {
    const root = document.documentElement;
    root.dataset.theme = theme;
    root.dataset.colorMode = colorMode;
    root.classList.toggle('dark', colorMode === 'dark');
    root.style.colorScheme = colorMode;
    if (typography && typography.fontFamily !== 'theme') {
      root.style.setProperty('--app-font-family', FONT_FAMILIES[typography.fontFamily]);
    } else {
      root.style.removeProperty('--app-font-family');
    }
    if (typography?.fontSize != null) {
      root.style.setProperty('--app-font-size', `${typography.fontSize}px`);
    } else {
      root.style.removeProperty('--app-font-size');
    }
    if (typography?.fontWeight != null) {
      root.style.setProperty('--app-font-weight', String(typography.fontWeight));
    } else {
      root.style.removeProperty('--app-font-weight');
    }
    let ligatures = 'common-ligatures contextual';
    if (typography?.ligatures === false) ligatures = 'no-common-ligatures no-contextual';
    root.style.setProperty('--app-font-ligatures', ligatures);
  }, [theme, colorMode, typography]);

  return colorMode;
}
