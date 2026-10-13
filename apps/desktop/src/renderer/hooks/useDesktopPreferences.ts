import { useCallback, useEffect, useRef, useState } from 'react';
import { desktopApi } from '../api/desktop';
import type { PreferencesSnapshot } from '../types/preferences';

export function useDesktopPreferences() {
  const [snapshot, setSnapshot] = useState<PreferencesSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const revisionRef = useRef(0);
  const refresh = useCallback(async () => {
    const revision = revisionRef.current + 1;
    revisionRef.current = revision;
    try {
      const next = await desktopApi.getDesktopPreferences();
      if (revision !== revisionRef.current) return;
      setSnapshot(next);
      setError(null);
    } catch (cause) {
      if (revision === revisionRef.current) setError(String(cause));
    }
  }, []);

  useEffect(() => {
    let disposed = false;
    const unlisten = desktopApi.onDesktopPreferences((preferences) => {
      if (disposed) return;
      setSnapshot((current) => current && { ...current, preferences });
      void refresh();
    });
    const directoryUnlisten = desktopApi.onSettingsDirectoryChanged(() => {
      if (!disposed) void refresh();
    });
    void refresh();
    return () => {
      disposed = true;
      revisionRef.current += 1;
      void unlisten.then((stop) => stop());
      void directoryUnlisten.then((stop) => stop());
    };
  }, [refresh]);

  return { snapshot, setSnapshot, error, refresh };
}
