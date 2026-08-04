import { useCallback, useEffect, useState } from 'react';
import { desktopApi } from '../api/desktop';

export function useOnboarding() {
  const [completed, setCompleted] = useState(false);
  const [initialised, setInitialised] = useState(false);

  useEffect(() => {
    let cancelled = false;
    desktopApi.getOnboardingCompleted().then((v) => {
      if (!cancelled) {
        setCompleted(v);
        setInitialised(true);
      }
    });
    return () => {
      cancelled = true;
    };
  }, []);

  const dismiss = useCallback(async () => {
    const ok = await desktopApi.setOnboardingCompleted();
    if (ok) setCompleted(true);
  }, []);

  return { completed: !initialised || completed, dismiss };
}
