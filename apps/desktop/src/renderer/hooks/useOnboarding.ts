import { useCallback, useSyncExternalStore } from 'react';

const STORAGE_KEY = 'slopcast-onboarding-completed';

const readCompleted = (): boolean => {
  try {
    return localStorage.getItem(STORAGE_KEY) === 'true';
  } catch {
    return false;
  }
};

const ONBOARDING_STORE = {
  listeners: new Set<() => void>(),
  subscribe: (cb: () => void) => {
    ONBOARDING_STORE.listeners.add(cb);
    return () => {
      ONBOARDING_STORE.listeners.delete(cb);
    };
  },
  getSnapshot: () => readCompleted(),
  notify: () => {
    for (const l of ONBOARDING_STORE.listeners) {
      l();
    }
  },
};

export function useOnboarding() {
  const completed = useSyncExternalStore(ONBOARDING_STORE.subscribe, ONBOARDING_STORE.getSnapshot);

  const dismiss = useCallback(() => {
    try {
      localStorage.setItem(STORAGE_KEY, 'true');
    } catch {
      // Storage unavailable — retry on next mount
    }
    ONBOARDING_STORE.notify();
  }, []);

  const reset = useCallback(() => {
    try {
      localStorage.removeItem(STORAGE_KEY);
    } catch {
      // Storage unavailable
    }
    ONBOARDING_STORE.notify();
  }, []);

  return { completed, dismiss, reset };
}
