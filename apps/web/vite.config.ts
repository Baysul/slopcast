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
      '@': path.resolve(import.meta.dirname, './src'),
    },
  },
  build: {
    sourcemap: false,
    minify: 'oxc',
    cssMinify: true,
    reportCompressedSize: false,
    target: 'esnext',
    rolldownOptions: {
      output: {
        minify: {
          compress: { dropDebugger: true },
          codegen: { legalComments: 'none' },
        },
      },
    },
  },
  server: {
    host: '0.0.0.0',
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
