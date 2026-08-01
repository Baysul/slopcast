import type React from 'react';
import { useEffect, useRef } from 'react';

const WIDTH = 44;
const HEIGHT = 14;
const BARS = 12;
const ACTIVE_THRESHOLD = 0.0003; // -66 dBFS noise floor

interface AudioLevelMeterProps {
  level: number;
}

// Transform raw linear peak [0, 1] into a perceptually scaled, auto-gained value [0, 1]
function scaleAudioLevel(rawLevel: number, adaptivePeak: number): number {
  if (rawLevel < ACTIVE_THRESHOLD) return 0;

  // Compute adaptive gain boost for low-volume streams
  const effectivePeak = Math.max(ACTIVE_THRESHOLD, adaptivePeak);
  const gain = Math.min(10.0, 0.2 / effectivePeak);
  const boosted = Math.min(1.0, rawLevel * gain);

  // Perceptual power curve (v^0.38) maps decibel-like dynamic range smoothly
  return boosted ** 0.38;
}

// Scrolling peak-history bar meter fed by native PipeWire metering.
export const AudioLevelMeter: React.FC<AudioLevelMeterProps> = ({ level }) => {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const historyRef = useRef<number[]>([]);
  const adaptivePeakRef = useRef<number>(0.05);

  useEffect(() => {
    if (level > adaptivePeakRef.current) {
      adaptivePeakRef.current = level;
    } else {
      adaptivePeakRef.current = Math.max(0.001, adaptivePeakRef.current * 0.98);
    }

    const scaled = scaleAudioLevel(level, adaptivePeakRef.current);

    const history = historyRef.current;
    history.push(scaled);
    if (history.length > BARS) {
      history.splice(0, history.length - BARS);
    }

    const canvas = canvasRef.current;
    const ctx = canvas?.getContext('2d');
    if (!canvas || !ctx) return;

    const dpr = window.devicePixelRatio || 1;
    if (canvas.width !== WIDTH * dpr || canvas.height !== HEIGHT * dpr) {
      canvas.width = WIDTH * dpr;
      canvas.height = HEIGHT * dpr;
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, WIDTH, HEIGHT);

    const gap = 1.5;
    const barWidth = (WIDTH - gap * (BARS - 1)) / BARS;
    for (let i = 0; i < BARS; i++) {
      const v = history[i] ?? 0;
      const x = i * (barWidth + gap);

      if (v > 0) {
        const barHeight = Math.max(2.0, Math.min(1.0, v) * HEIGHT);
        const alpha = 0.4 + Math.min(1.0, v) * 0.6;
        ctx.fillStyle = `rgba(196, 128, 74, ${alpha})`;
        ctx.beginPath();
        ctx.roundRect(x, HEIGHT - barHeight, barWidth, barHeight, 1);
        ctx.fill();
      } else {
        ctx.fillStyle = 'rgba(255, 255, 255, 0.08)';
        ctx.beginPath();
        ctx.roundRect(x, HEIGHT - 1.5, barWidth, 1.5, 1);
        ctx.fill();
      }
    }
  }, [level]);

  return <canvas ref={canvasRef} style={{ width: WIDTH, height: HEIGHT }} className="shrink-0" />;
};
