import { AlertTriangle, CheckCircle2, Info, X } from 'lucide-react';
import type React from 'react';
import { useCallback, useRef, useState } from 'react';
import { twMerge } from 'tailwind-merge';

export type ToastVariant = 'success' | 'info' | 'error';

export interface ToastData {
  id: number;
  title: string;
  description?: string;
  variant: ToastVariant;
  duration: number;
  icon?: React.ReactNode;
}

export interface ToastInput {
  title: string;
  description?: string;
  variant?: ToastVariant;
  duration?: number;
  icon?: React.ReactNode;
}

const DEFAULT_DURATION = 4200;
const MAX_VISIBLE = 3;
const SOUND_THROTTLE_MS = 350;

// ── Notification sound (Web Audio API) ─────────────────────────────────
// A short synthesized chime — no audio asset is shipped. The AudioContext is
// created lazily and resumed; it must be primed during a user gesture (see
// primeAudioContext) so it can later play from non-gesture events such as a
// spectator connecting over WebSocket.

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

export const playNotificationSound = (variant: ToastVariant = 'success'): void => {
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

export const useToasts = () => {
  const [toasts, setToasts] = useState<ToastData[]>([]);
  const idRef = useRef(0);
  const lastSoundRef = useRef(0);

  const dismiss = useCallback((id: number) => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
  }, []);

  const push = useCallback(
    (input: ToastInput) => {
      const id = ++idRef.current;
      const toast: ToastData = {
        id,
        title: input.title,
        description: input.description,
        variant: input.variant ?? 'info',
        duration: input.duration ?? DEFAULT_DURATION,
        icon: input.icon,
      };
      setToasts((prev) => {
        const next = [...prev, toast];
        return next.length > MAX_VISIBLE ? next.slice(next.length - MAX_VISIBLE) : next;
      });
      // Throttle the chime so a burst of connections plays one cascade rather
      // than a stutter of overlapping tones.
      const now = Date.now();
      if (now - lastSoundRef.current >= SOUND_THROTTLE_MS) {
        lastSoundRef.current = now;
        playNotificationSound(toast.variant);
      }
      if (toast.duration > 0) {
        window.setTimeout(() => dismiss(id), toast.duration);
      }
      return id;
    },
    [dismiss],
  );

  return { toasts, push, dismiss };
};

const VARIANT_ICON: Record<ToastVariant, React.ReactNode> = {
  success: <CheckCircle2 className="h-5 w-5" aria-hidden="true" />,
  info: <Info className="h-5 w-5" aria-hidden="true" />,
  error: <AlertTriangle className="h-5 w-5" aria-hidden="true" />,
};

const VARIANT_ACCENT: Record<ToastVariant, string> = {
  success: 'text-safelight',
  info: 'text-gray-300',
  error: 'text-destructive',
};

const ToastItem: React.FC<{ toast: ToastData; onDismiss: () => void }> = ({ toast, onDismiss }) => {
  const icon = toast.icon ?? VARIANT_ICON[toast.variant];
  // Error toasts announce assertively; success/info are polite status updates.
  const liveRole = toast.variant === 'error' ? 'alert' : 'status';
  return (
    <div
      role={liveRole}
      className="animate-toast-in pointer-events-auto flex items-start gap-3 rounded-xl border border-gray-800/70 bg-popover/95 p-3.5 pr-2 shadow-lg shadow-black/40 backdrop-blur-md"
    >
      <span className={twMerge('mt-0.5 shrink-0', VARIANT_ACCENT[toast.variant])}>{icon}</span>
      <div className="min-w-0 flex-1 space-y-0.5">
        <p className="text-sm font-semibold leading-tight text-gray-100">{toast.title}</p>
        {toast.description && <p className="text-xs leading-snug text-gray-400">{toast.description}</p>}
      </div>
      <button
        type="button"
        onClick={onDismiss}
        aria-label="Dismiss notification"
        className="-mr-0.5 shrink-0 rounded-md p-1 text-gray-500 transition-colors hover:bg-gray-700/50 hover:text-gray-300 focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70"
      >
        <X className="h-4 w-4" aria-hidden="true" />
      </button>
    </div>
  );
};

export const ToastViewport: React.FC<{ toasts: ToastData[]; onDismiss: (id: number) => void }> = ({
  toasts,
  onDismiss,
}) => (
  <div className="pointer-events-none fixed bottom-4 right-4 z-50 flex w-[min(92vw,360px)] flex-col gap-2">
    {toasts.map((t) => (
      <ToastItem key={t.id} toast={t} onDismiss={() => onDismiss(t.id)} />
    ))}
  </div>
);
