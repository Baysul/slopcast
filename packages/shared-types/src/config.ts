import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';
import type { AppConfig } from './index.js';

export type { AppConfig };

export function loadConfig(): AppConfig {
  const serverPort = parseInt(process.env.SERVER_PORT || process.env.PORT || '', 10) || 0;
  const webPort = parseInt(process.env.WEB_PORT || '', 10) || 0;
  const apiEndpoint = process.env.API_ENDPOINT || '';
  const websiteUrl = process.env.WEBSITE_URL || '';
  const livekitUrl = process.env.LIVEKIT_URL || '';
  const livekitApiKey = process.env.LIVEKIT_API_KEY || '';
  const livekitApiSecret = process.env.LIVEKIT_API_SECRET || '';

  const defaults: AppConfig = {
    serverPort: serverPort || 3001,
    webPort: webPort || 3000,
    apiEndpoint: apiEndpoint || `http://localhost:${serverPort || 3001}`,
    websiteUrl: websiteUrl || `http://localhost:${webPort || 3000}`,
    livekitUrl: livekitUrl || 'ws://localhost:7880',
    livekitApiKey: livekitApiKey || 'devkey',
    livekitApiSecret: livekitApiSecret || 'secret',
  };

  const configPath = path.resolve(process.cwd(), 'slopcast.config.json');

  if (existsSync(configPath)) {
    try {
      const fileConfig = JSON.parse(readFileSync(configPath, 'utf-8')) as Partial<AppConfig>;
      return {
        serverPort: serverPort || fileConfig.serverPort || defaults.serverPort,
        webPort: webPort || fileConfig.webPort || defaults.webPort,
        apiEndpoint:
          apiEndpoint || fileConfig.apiEndpoint || `http://localhost:${serverPort || fileConfig.serverPort || 3001}`,
        websiteUrl: websiteUrl || fileConfig.websiteUrl || `http://localhost:${webPort || fileConfig.webPort || 3000}`,
        livekitUrl: livekitUrl || fileConfig.livekitUrl || defaults.livekitUrl,
        livekitApiKey: livekitApiKey || fileConfig.livekitApiKey || defaults.livekitApiKey,
        livekitApiSecret: livekitApiSecret || fileConfig.livekitApiSecret || defaults.livekitApiSecret,
      };
    } catch {
      console.error('Failed to parse slopcast.config.json, using env/defaults');
    }
  }

  return defaults;
}
