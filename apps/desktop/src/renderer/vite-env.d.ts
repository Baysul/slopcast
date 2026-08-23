/// <reference types="vite/client" />

interface Window {
  __PREVIEW_BENCH__?: boolean;
  __PREVIEW_RENDERER__?: 'webgpu' | 'webgl2' | 'canvas2d' | 'none';
  __PREVIEW_BENCH_DATA__?: Array<[number, number, number | null]>;
}
