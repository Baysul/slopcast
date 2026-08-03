import type React from 'react';
import { useEffect, useRef } from 'react';

interface AudioVisualizerProps {
  mediaStream: MediaStream | null;
  showStatus?: boolean;
}

const AUDIO_UNLOCK_EVENT = 'slopcast-audio-unlock';

export function unlockAudioContexts() {
  window.dispatchEvent(new CustomEvent(AUDIO_UNLOCK_EVENT));
}

export const AudioVisualizer: React.FC<AudioVisualizerProps> = ({ mediaStream, showStatus }) => {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  useEffect(() => {
    if (!mediaStream || mediaStream.getAudioTracks().length === 0) return;

    let animationFrameId: number;
    let audioCtx: AudioContext | null = null;
    let started = false;

    const startVisualizer = () => {
      if (started || !mediaStream || mediaStream.getAudioTracks().length === 0) return;

      try {
        const ACtor =
          window.AudioContext ?? (window as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
        if (!ACtor) return;
        audioCtx = new ACtor();
        if (audioCtx.state === 'suspended') {
          audioCtx.resume().catch((err) => {
            console.warn('[AudioVisualizer] AudioContext resume failed:', err);
          });
        }

        const analyser = audioCtx.createAnalyser();
        analyser.fftSize = 64;

        const source = audioCtx.createMediaStreamSource(mediaStream);
        source.connect(analyser);

        const bufferLength = analyser.frequencyBinCount;
        const dataArray = new Uint8Array(bufferLength);

        const canvas = canvasRef.current;
        if (!canvas) return;

        const ctx = canvas.getContext('2d');
        if (!ctx) return;

        // DPR-aware backing store so the 80×20 canvas is crisp on HiDPI
        // displays; the CSS size stays fixed.
        const dpr = window.devicePixelRatio || 1;
        canvas.width = 80 * dpr;
        canvas.height = 20 * dpr;
        ctx.scale(dpr, dpr);

        let wasSilent = false;

        const draw = () => {
          animationFrameId = requestAnimationFrame(draw);
          if (document.hidden) return;

          analyser.getByteFrequencyData(dataArray);

          let hasSignal = false;
          for (let i = 0; i < bufferLength; i++) {
            if (dataArray[i] > 0) {
              hasSignal = true;
              break;
            }
          }

          if (!hasSignal) {
            if (!wasSilent) {
              ctx.clearRect(0, 0, 80, 20);
              wasSilent = true;
            }
            return;
          }
          wasSilent = false;

          ctx.clearRect(0, 0, 80, 20);

          const barWidth = (80 / bufferLength) * 1.5;
          let x = 0;

          for (let i = 0; i < bufferLength; i++) {
            const barHeight = (dataArray[i] / 255) * 20;
            const alpha = 0.3 + (dataArray[i] / 255) * 0.7;
            ctx.fillStyle = `rgba(196, 128, 74, ${alpha})`;
            ctx.fillRect(x, 20 - barHeight, barWidth - 2, barHeight);
            x += barWidth + 1;
          }
        };

        // Only mark as started after the full pipeline is live, so a failed
        // first attempt (no AudioContext/canvas/ctx) can retry on unlock.
        started = true;
        draw();
      } catch (err) {
        console.error('[AudioVisualizer] Failed to initialize AudioContext:', err);
      }
    };

    startVisualizer();

    const onUnlock = () => {
      if (audioCtx && audioCtx.state === 'suspended') {
        audioCtx.resume().catch((err) => {
          console.warn('[AudioVisualizer] AudioContext resume failed:', err);
        });
      }
      startVisualizer();
    };
    window.addEventListener(AUDIO_UNLOCK_EVENT, onUnlock);

    return () => {
      window.removeEventListener(AUDIO_UNLOCK_EVENT, onUnlock);
      if (animationFrameId) cancelAnimationFrame(animationFrameId);
      if (audioCtx && audioCtx.state !== 'closed') {
        audioCtx.close().catch((err) => {
          console.warn('[AudioVisualizer] AudioContext close failed:', err);
        });
      }
    };
  }, [mediaStream]);

  return (
    <div className="flex items-center gap-2 bg-black/30 backdrop-blur-sm px-3 py-1.5 rounded-lg border border-white/10">
      {showStatus && (
        <span className="relative w-1.5 h-1.5">
          <span className="absolute inset-0 rounded-full bg-safelight animate-ping opacity-75" />
          <span className="absolute inset-0 rounded-full bg-safelight" />
        </span>
      )}
      <span className="text-xs font-medium text-white/50 uppercase tracking-wider">Audio</span>
      <canvas ref={canvasRef} width={80} height={20} className="rounded overflow-hidden" />
    </div>
  );
};
