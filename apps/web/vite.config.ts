import path from 'node:path';
import { loadConfig } from '@slopcast/shared-types/config';
import tailwindcss from '@tailwindcss/vite';
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

const config = loadConfig();

export default defineConfig({
  plugins: [tailwindcss(), react()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
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
