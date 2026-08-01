import path from 'node:path';
import tailwindcss from '@tailwindcss/vite';
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

export default defineConfig({
  plugins: [tailwindcss(), react()],
  root: path.resolve(__dirname, 'src/renderer'),
  base: './',
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src/renderer'),
    },
  },
  build: {
    outDir: path.resolve(__dirname, 'dist/renderer'),
    emptyOutDir: true,
    sourcemap: false,
    minify: 'esbuild',
    cssMinify: true,
    reportCompressedSize: false,
    target: 'esnext',
    rollupOptions: {
      treeshake: {
        preset: 'recommended',
      },
    },
  },
  esbuild: {
    legalComments: 'none',
    drop: ['debugger'],
  },
  server: {
    port: 5173,
    strictPort: true,
  },
});
