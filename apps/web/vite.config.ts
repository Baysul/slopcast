import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import path from 'path';

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  server: {
    port: 3000,
    proxy: {
      // Tunnel WebSocket + REST to the signaling server so the browser
      // stays on a single origin (avoids cross-port / private-network blocks).
      '/ws': {
        target: 'http://localhost:3001',
        ws: true,
        rewrite: () => '/',
      },
      '/api': {
        target: 'http://localhost:3001',
        changeOrigin: true,
      },
      '/health': {
        target: 'http://localhost:3001',
        changeOrigin: true,
      },
    },
  },
});

