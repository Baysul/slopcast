declare global {
  interface Window {
    webkitAudioContext?: typeof AudioContext;
  }
}

let audioCtx: AudioContext | null = null;

const getAudioCtx = (): AudioContext | null => {
  const Ctor = typeof window.AudioContext === 'function' ? window.AudioContext : window.webkitAudioContext;
  if (Ctor === undefined) {
    return null;
  }
  try {
    audioCtx ??= new Ctor();
    if (audioCtx.state === 'suspended') {
      void audioCtx.resume();
    }
    return audioCtx;
  } catch (err) {
    console.warn('AudioContext creation failed:', err);
    return null;
  }
};

export const primeAudioContext = (): void => {
  getAudioCtx();
};

interface ToneSpec {
  freq: number;
  start: number;
  dur: number;
  type?: OscillatorType;
  peak?: number;
}

const playChime = (tones: ToneSpec[]): void => {
  const ctx = getAudioCtx();
  if (!ctx) {
    return;
  }
  const now = ctx.currentTime;
  const master = ctx.createGain();
  master.gain.setValueAtTime(0.0001, now);
  master.gain.exponentialRampToValueAtTime(0.2, now + 0.012);
  master.gain.exponentialRampToValueAtTime(0.0001, now + 0.6);
  master.connect(ctx.destination);
  let lastOsc: OscillatorNode | null = null;
  for (const t of tones) {
    const osc = ctx.createOscillator();
    osc.type = t.type ?? 'sine';
    osc.frequency.setValueAtTime(t.freq, now + t.start);
    const g = ctx.createGain();
    const peak = t.peak ?? 0.9;
    g.gain.setValueAtTime(0.0001, now + t.start);
    g.gain.exponentialRampToValueAtTime(peak, now + t.start + 0.01);
    g.gain.exponentialRampToValueAtTime(0.0001, now + t.start + t.dur);
    osc.connect(g);
    g.connect(master);
    osc.start(now + t.start);
    osc.stop(now + t.start + t.dur + 0.03);
    lastOsc = osc;
  }
  if (lastOsc) {
    lastOsc.onended = () => {
      try {
        master.disconnect();
      } catch {
        console.warn('Audio master gain already disconnected');
      }
    };
  }
};

const SOUND_THROTTLE_MS = 350;

let lastSoundTime = 0;

export type NotificationVariant = 'success' | 'info' | 'error';

export const playNotificationSound = (variant: NotificationVariant): void => {
  const now = Date.now();
  if (now - lastSoundTime < SOUND_THROTTLE_MS) {
    return;
  }
  lastSoundTime = now;
  try {
    switch (variant) {
      case 'success':
        playChime([
          { dur: 0.15, freq: 659.25, start: 0 },
          { dur: 0.22, freq: 987.77, start: 0.075 },
        ]);
        break;
      case 'info':
        playChime([{ dur: 0.16, freq: 880, peak: 0.5, start: 0, type: 'triangle' }]);
        break;
      case 'error':
        playChime([
          { dur: 0.16, freq: 440, start: 0, type: 'triangle' },
          { dur: 0.24, freq: 349.23, start: 0.09, type: 'triangle' },
        ]);
        break;
    }
  } catch (err) {
    console.warn('Notification sound failed:', err);
  }
};
