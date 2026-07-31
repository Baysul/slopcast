import type React from 'react';
import { useEffect, useRef } from 'react';

const WIDTH = 44;
const HEIGHT = 14;
const BARS = 12;

interface AudioLevelMeterProps {
  level: number;
}

// Scrolling peak-history bar meter fed by native PipeWire metering (one scalar
// peak per poll), so no AudioContext is needed on the presenter side.
export const AudioLevelMeter: React.FC<AudioLevelMeterProps> = ({ level }) => {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const historyRef = useRef<number[]>([]);

  useEffect(() => {
    const history = historyRef.current;
    history.push(level);
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
      const barHeight = Math.max(1.5, Math.min(1, v) * HEIGHT);
      const x = i * (barWidth + gap);
      if (v > 0.02) {
        ctx.fillStyle = `rgba(196, 128, 74, ${0.35 + Math.min(1, v) * 0.65})`;
      } else {
        ctx.fillStyle = 'rgba(255, 255, 255, 0.08)';
      }
      ctx.beginPath();
      ctx.roundRect(x, HEIGHT - barHeight, barWidth, barHeight, 1);
      ctx.fill();
    }
  }, [level]);

  return <canvas ref={canvasRef} style={{ width: WIDTH, height: HEIGHT }} className="shrink-0" />;
};
