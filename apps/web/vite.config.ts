import path from 'node:path';

import { loadConfig } from '@slopcast/shared-types/config';
import tailwindcss from '@tailwindcss/vite';
import react from '@vitejs/plugin-react';
import { defineConfig, lazyPlugins } from 'vite-plus';

process.env.ALLOW_DEV_KEYS = 'true';
const config = loadConfig();

export default defineConfig({
  build: {
    cssMinify: true,
    minify: 'oxc',
    reportCompressedSize: false,
    rolldownOptions: {
      output: {
        minify: {
          codegen: { legalComments: 'none' },
          compress: { dropDebugger: true },
        },
      },
    },
    sourcemap: false,
    target: 'esnext',
  },
  plugins: lazyPlugins(() => [tailwindcss(), react()]),
  resolve: {
    alias: {
      '@': path.resolve(import.meta.dirname, './src'),
    },
  },
  server: {
    host: '0.0.0.0',
    port: config.webPort,
    proxy: {
      '/api': {
        changeOrigin: true,
        target: config.apiEndpoint,
      },
      '/health': {
        changeOrigin: true,
        target: config.apiEndpoint,
      },
    },
  },
});
