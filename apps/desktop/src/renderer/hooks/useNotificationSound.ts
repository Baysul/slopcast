let audioCtx: AudioContext | null = null;

const getAudioCtx = (): AudioContext | null => {
  if (typeof window === 'undefined') return null;
  const Ctor =
    window.AudioContext ?? (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
  if (!Ctor) return null;
  try {
    if (!audioCtx) audioCtx = new Ctor();
    if (audioCtx.state === 'suspended') void audioCtx.resume();
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
  if (!ctx) return;
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
  if (now - lastSoundTime < SOUND_THROTTLE_MS) return;
  lastSoundTime = now;
  try {
    switch (variant) {
      case 'success':
        playChime([
          { freq: 659.25, start: 0, dur: 0.15 },
          { freq: 987.77, start: 0.075, dur: 0.22 },
        ]);
        break;
      case 'info':
        playChime([{ freq: 880, start: 0, dur: 0.16, peak: 0.5, type: 'triangle' }]);
        break;
      case 'error':
        playChime([
          { freq: 440, start: 0, dur: 0.16, type: 'triangle' },
          { freq: 349.23, start: 0.09, dur: 0.24, type: 'triangle' },
        ]);
        break;
    }
  } catch (err) {
    console.warn('Notification sound failed:', err);
  }
};
