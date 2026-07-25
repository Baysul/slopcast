import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { createServer } from './server';

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
    path.resolve(process.cwd(), 'screen-share.config.json'),
    path.resolve(__dirname, '../../../../screen-share.config.json'),
  ];

  for (const p of candidates) {
    if (existsSync(p)) {
      try {
        return { ...defaults, ...JSON.parse(readFileSync(p, 'utf-8')) };
      } catch {
        console.warn(`[server] Failed to parse config at ${p}, using defaults`);
      }
    }
  }

  console.warn('[server] No screen-share.config.json found, using defaults');
  return defaults;
}

const config = loadConfig();

const PORT = process.env.PORT ? parseInt(process.env.PORT, 10) : config.serverPort;
const BASE_URL = process.env.BASE_URL || config.websiteUrl;

const { server } = createServer(PORT, BASE_URL);

server.listen(PORT, () => {
  console.log(`Signaling & Room Server listening on :${PORT}`);
  console.log(`WebSocket: ws://localhost:${PORT}`);
  console.log(`Room share base URL: ${BASE_URL}`);
});
