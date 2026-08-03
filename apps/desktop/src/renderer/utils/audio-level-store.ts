type WaveCallback = (columns: number[]) => void;

export const WAVE_COLUMN_COUNT = 96;
const NOTIFY_EPSILON = 0.002;

// Columns above this amplitude count as active (silence is exactly zero).
const ACTIVE_AMP = 0.002;

export function silentWave(): number[] {
  return new Array(WAVE_COLUMN_COUNT * 2).fill(0);
}

export function waveIsActive(columns: number[]): boolean {
  return columns.some((v) => Math.abs(v) > ACTIVE_AMP);
}

function waveChanged(prev: number[], next: number[]): boolean {
  if (prev.length !== next.length) return true;
  for (let i = 0; i < next.length; i++) {
    if (Math.abs((prev[i] ?? 0) - (next[i] ?? 0)) > NOTIFY_EPSILON) {
      return true;
    }
  }
  return false;
}

function accumulateMax(target: number[], columns: number[]): void {
  const pairs = Math.min(Math.floor(columns.length / 2), WAVE_COLUMN_COUNT);
  for (let i = 0; i < pairs; i++) {
    const min = columns[i * 2] ?? 0;
    const max = columns[i * 2 + 1] ?? 0;
    if (min < target[i * 2]) {
      target[i * 2] = min;
    }
    if (max > target[i * 2 + 1]) {
      target[i * 2 + 1] = max;
    }
  }
}

class AudioWaveStore {
  private waves = new Map<number, number[]>();
  private listeners = new Map<number, Set<WaveCallback>>();

  // Update waveforms silently in memory without triggering React DOM re-renders
  public updateWave(apps: Array<{ id: number; columns: number[] }>): void {
    const seen = new Set<number>();
    const maxPerColumn = silentWave();

    for (const { id, columns } of apps) {
      seen.add(id);
      // Paused/silent streams (all-zero columns) must not drive the Desktop
      // Audio meter, so only live streams are accumulated into its max.
      if (waveIsActive(columns)) {
        accumulateMax(maxPerColumn, columns);
      }

      const prev = this.waves.get(id);
      if (!prev || waveChanged(prev, columns)) {
        this.waves.set(id, columns);
        this.notify(id, columns);
      }
    }

    // Desktop Audio (id -1) mirrors the max column pair across all apps
    const prevMax = this.waves.get(-1);
    if (!prevMax || waveChanged(prevMax, maxPerColumn)) {
      this.waves.set(-1, maxPerColumn);
      this.notify(-1, maxPerColumn);
    }

    // Reset apps that stopped emitting
    for (const [id, prev] of this.waves.entries()) {
      if (id !== -1 && !seen.has(id)) {
        const silence = silentWave();
        if (waveChanged(prev, silence)) {
          this.waves.set(id, silence);
          this.notify(id, silence);
        }
      }
    }
  }

  public getWave(id: number): number[] {
    return this.waves.get(id) ?? silentWave();
  }

  // Subscribe directly per app ID
  public subscribe(id: number, callback: WaveCallback): () => void {
    let set = this.listeners.get(id);
    if (!set) {
      set = new Set();
      this.listeners.set(id, set);
    }
    set.add(callback);

    // Emit initial columns immediately
    callback(this.getWave(id));

    return () => {
      const currentSet = this.listeners.get(id);
      if (currentSet) {
        currentSet.delete(callback);
        if (currentSet.size === 0) {
          this.listeners.delete(id);
        }
      }
    };
  }

  private notify(id: number, columns: number[]): void {
    const set = this.listeners.get(id);
    if (set) {
      for (const cb of set) {
        cb(columns);
      }
    }
  }
}

export const audioWaveStore = new AudioWaveStore();
