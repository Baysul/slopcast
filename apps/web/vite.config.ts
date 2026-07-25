import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

interface AppConfig {
  serverPort: number;
  webPort: number;
  apiEndpoint: string;
  websiteUrl: string;
}

function loadConfig(): AppConfig {
  const defaults: AppConfig = {
    serverPort: 3001,
    webPort: 3000,
    apiEndpoint: 'http://localhost:3001',
    websiteUrl: 'http://localhost:3000',
  };

  const candidates = [
    path.resolve(__dirname, '../../../slopcast.config.json'),
    path.resolve(process.cwd(), 'slopcast.config.json'),
  ];

  for (const p of candidates) {
    if (existsSync(p)) {
      try {
        return { ...defaults, ...JSON.parse(readFileSync(p, 'utf-8')) };
      } catch {
        /* use defaults */
      }
    }
  }

  return defaults;
}

const config = loadConfig();

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  server: {
    port: config.webPort,
    proxy: {
      '/ws': {
        target: config.apiEndpoint,
        ws: true,
        rewrite: () => '/',
      },
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
