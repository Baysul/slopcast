import React, { useEffect, useRef } from 'react';

interface AudioVisualizerProps {
  mediaStream: MediaStream | null;
  className?: string;
}

export const AudioVisualizer: React.FC<AudioVisualizerProps> = ({ mediaStream, className }) => {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  useEffect(() => {
    if (!mediaStream || mediaStream.getAudioTracks().length === 0) {
      return;
    }

    let animationFrameId: number;
    let audioCtx: AudioContext | null = null;

    try {
      audioCtx = new (window.AudioContext || (window as any).webkitAudioContext)();
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

      const draw = () => {
        animationFrameId = requestAnimationFrame(draw);
        analyser.getByteFrequencyData(dataArray);

        ctx.clearRect(0, 0, canvas.width, canvas.height);

        const barWidth = (canvas.width / bufferLength) * 1.5;
        let x = 0;

        for (let i = 0; i < bufferLength; i++) {
          const barHeight = (dataArray[i] / 255) * canvas.height;

          // Gradient color from indigo to emerald based on intensity
          const gradient = ctx.createLinearGradient(0, canvas.height, 0, 0);
          gradient.addColorStop(0, 'rgba(99, 102, 241, 0.4)');
          gradient.addColorStop(1, 'rgba(16, 185, 129, 0.9)');

          ctx.fillStyle = gradient;
          ctx.fillRect(x, canvas.height - barHeight, barWidth - 2, barHeight);

          x += barWidth + 1;
        }
      };

      draw();
    } catch (err) {
      console.error('[AudioVisualizer] Failed to initialize AudioContext:', err);
    }

    return () => {
      if (animationFrameId) {
        cancelAnimationFrame(animationFrameId);
      }
      if (audioCtx && audioCtx.state !== 'closed') {
        audioCtx.close().catch(() => {});
      }
    };
  }, [mediaStream]);

  return (
    <div className={`flex items-center gap-2 bg-gray-900/80 px-3 py-1.5 rounded-lg border border-gray-800 ${className || ''}`}>
      <span className="text-xs font-medium text-gray-400 uppercase tracking-wider">Audio</span>
      <canvas
        ref={canvasRef}
        width={80}
        height={20}
        className="rounded overflow-hidden"
      />
    </div>
  );
};
