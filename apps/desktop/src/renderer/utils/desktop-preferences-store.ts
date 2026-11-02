import { desktopApi } from '../api/desktop';
import type { DesktopPreferences, PreferencesSnapshot } from '../types/preferences';

type Listener = () => void;

export interface DesktopPreferencesState {
  readonly error: string | null;
  readonly snapshot: PreferencesSnapshot | null;
}

const initialPreferencesState: DesktopPreferencesState = { error: null, snapshot: null };

/**
 * Snapshot store for desktop preferences.
 *
 * The Tauri backend owns the preferences; this store mirrors them so components can read them with
 * `useSyncExternalStore` instead of loading them from an effect.
 */
class DesktopPreferencesStore {
  private state: DesktopPreferencesState = initialPreferencesState;
  private readonly listeners = new Set<Listener>();
  private readonly stopListening: (() => void)[] = [];
  private generation = 0;
  private revision = 0;
  private listenerCount = 0;

  public getSnapshot = (): DesktopPreferencesState => this.state;

  public subscribe = (listener: Listener): (() => void) => {
    this.listeners.add(listener);
    this.listenerCount += 1;
    if (this.listenerCount === 1) {
      this.begin();
    }

    return () => {
      this.listeners.delete(listener);
      this.listenerCount -= 1;
      if (this.listenerCount === 0) {
        this.end();
      }
    };
  };

  public setSnapshot = (next: PreferencesSnapshot | null): void => {
    this.emit({ error: this.state.error, snapshot: next });
  };

  public refresh = async (): Promise<void> => {
    const revision = this.revision + 1;
    this.revision = revision;
    try {
      const next = await desktopApi.getDesktopPreferences();
      if (revision !== this.revision) {
        return;
      }
      this.emit({ error: null, snapshot: next });
    } catch (cause) {
      if (revision === this.revision) {
        this.emit({ error: String(cause), snapshot: this.state.snapshot });
      }
    }
  };

  private emit(next: DesktopPreferencesState): void {
    this.state = next;
    for (const listener of this.listeners) {
      listener();
    }
  }

  private begin(): void {
    const { generation } = this;
    void this.refresh();
    void desktopApi
      .onDesktopPreferences((preferences: DesktopPreferences) => {
        if (generation !== this.generation) {
          return;
        }
        const current = this.state.snapshot;
        this.emit({
          error: this.state.error,
          snapshot: current ? { ...current, preferences } : current,
        });
        void this.refresh();
      })
      .then((stop) => {
        this.keepStop(generation, stop);
      });
    void desktopApi
      .onSettingsDirectoryChanged(() => {
        if (generation === this.generation) {
          void this.refresh();
        }
      })
      .then((stop) => {
        this.keepStop(generation, stop);
      });
  }

  private end(): void {
    this.generation += 1;
    this.revision += 1;
    for (const stop of this.stopListening.splice(0)) {
      stop();
    }
  }

  private keepStop(generation: number, stop: () => void): void {
    if (generation === this.generation) {
      this.stopListening.push(stop);
      return;
    }
    stop();
  }
}

export const desktopPreferencesStore = new DesktopPreferencesStore();
