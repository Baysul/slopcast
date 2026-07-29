import type React from 'react';

export const Sparkline: React.FC<{ data: number[]; width?: number; height?: number }> = ({
  data,
  width = 88,
  height = 22,
}) => {
  let points = '';
  if (data.length >= 2) {
    const max = Math.max(...data);
    const min = Math.min(...data, 0);
    const span = Math.max(max - min, 0.001);
    const step = width / (data.length - 1);
    points = data
      .map((v, i) => {
        const x = i * step;
        const y = height - ((v - min) / span) * (height - 3) - 1.5;
        return `${x.toFixed(1)},${y.toFixed(1)}`;
      })
      .join(' ');
  }
  return (
    <svg
      width={width}
      height={height}
      viewBox={`0 0 ${width} ${height}`}
      className="block text-safelight"
      aria-hidden="true"
      preserveAspectRatio="none"
    >
      {points && (
        <polyline
          points={points}
          fill="none"
          stroke="currentColor"
          strokeWidth={1.5}
          strokeLinejoin="round"
          strokeLinecap="round"
          vectorEffect="non-scaling-stroke"
        />
      )}
    </svg>
  );
};
