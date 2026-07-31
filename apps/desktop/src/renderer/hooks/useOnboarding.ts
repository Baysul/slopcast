import { useCallback, useEffect, useState } from 'react';

export function useOnboarding() {
  const [completed, setCompleted] = useState(false);
  const [initialised, setInitialised] = useState(false);

  useEffect(() => {
    let cancelled = false;
    window.electronAPI?.getOnboardingCompleted().then((v) => {
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
    const ok = await window.electronAPI?.setOnboardingCompleted();
    if (ok) setCompleted(true);
  }, []);

  return { completed: !initialised || completed, dismiss };
}
