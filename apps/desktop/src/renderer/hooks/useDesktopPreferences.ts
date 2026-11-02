import { useSyncExternalStore } from 'react';

import { desktopPreferencesStore } from '../utils/desktop-preferences-store';
import { useAppearance } from './useAppearance';

export const useDesktopPreferences = () => {
  const { error, snapshot } = useSyncExternalStore(
    desktopPreferencesStore.subscribe,
    desktopPreferencesStore.getSnapshot,
  );
  const colorMode = useAppearance(snapshot?.preferences);

  return {
    colorMode,
    error,
    refresh: desktopPreferencesStore.refresh,
    setSnapshot: desktopPreferencesStore.setSnapshot,
    snapshot,
  };
};
