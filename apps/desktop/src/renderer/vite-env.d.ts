/// <reference types="vite/client" />

interface Window {
  previewBench?: boolean;
  previewRenderer?: 'webgpu' | 'webgl2' | 'canvas2d' | 'none';
  previewBenchData?: [number, number, number | null][];
}
