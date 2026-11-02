import type { AudioApp } from '@slopcast/shared-types';

import { desktopApi } from '../api/desktop';
import { audioAppsEqual } from './audio-grouping';

const AUDIO_APPS_POLL_MS = 3000;
const NO_SUBSCRIBERS = 0;
const FIRST_SUBSCRIBER = 1;

type Listener = () => void;

/**
 * Polling store for the capturable audio applications.
 *
 * Components read the list with `useSyncExternalStore`; polling only runs while at least one
 * component is subscribed, and pauses while the window is hidden.
 */
class AudioAppsStore {
  private apps: AudioApp[] = [];
  private readonly listeners = new Set<Listener>();
  private interval: ReturnType<typeof setInterval> | null = null;
  private subscriberCount = NO_SUBSCRIBERS;

  public getSnapshot = (): AudioApp[] => this.apps;

  public subscribe = (listener: Listener): (() => void) => {
    this.listeners.add(listener);
    this.subscriberCount += 1;
    if (this.subscriberCount === FIRST_SUBSCRIBER) {
      this.begin();
    }

    return () => {
      this.listeners.delete(listener);
      this.subscriberCount -= 1;
      if (this.subscriberCount === NO_SUBSCRIBERS) {
        this.end();
      }
    };
  };

  public refresh = async (): Promise<void> => {
    const apps = await desktopApi.getAudioApps();
    if (audioAppsEqual(this.apps, apps)) {
      return;
    }
    this.apps = apps;
    for (const listener of this.listeners) {
      listener();
    }
  };

  private begin(): void {
    void this.refresh();
    this.interval = setInterval(() => {
      if (document.visibilityState === 'visible') {
        void this.refresh();
      }
    }, AUDIO_APPS_POLL_MS);
  }

  private end(): void {
    if (this.interval !== null) {
      clearInterval(this.interval);
      this.interval = null;
    }
  }
}

export const audioAppsStore = new AudioAppsStore();
