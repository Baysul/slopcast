import path from 'node:path';
import { loadConfig } from '@slopcast/shared-types/config';
import tailwindcss from '@tailwindcss/vite';
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

process.env.ALLOW_DEV_KEYS = 'true';
const config = loadConfig();

export default defineConfig({
  plugins: [tailwindcss(), react()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  build: {
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
    port: config.webPort,
    proxy: {
      '/api': {
        target: config.apiEndpoint,
        changeOrigin: true,
      },
      '/health': {
        target: config.apiEndpoint,
        changeOrigin: true,
      },
    },
  },
});
