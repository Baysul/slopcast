import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';
import type { AppConfig } from './index.js';

export type { AppConfig };

function findConfigFile(): string | null {
  let dir = path.resolve(process.cwd());
  const root = path.parse(dir).root;
  while (true) {
    const candidate = path.join(dir, 'slopcast.config.json');
    if (existsSync(candidate)) return candidate;
    if (dir === root) break;
    dir = path.dirname(dir);
  }
  return null;
}

export function loadConfig(): AppConfig {
  const envServerPort = parseInt(process.env.SERVER_PORT || process.env.PORT || '', 10) || 0;
  const envWebPort = parseInt(process.env.WEB_PORT || '', 10) || 0;
  const envApiEndpoint = process.env.API_ENDPOINT || '';
  const envWebsiteUrl = process.env.WEBSITE_URL || '';
  const envLivekitUrl = process.env.LIVEKIT_URL || '';
  const envLivekitApiKey = process.env.LIVEKIT_API_KEY || '';
  const envLivekitApiSecret = process.env.LIVEKIT_API_SECRET || '';

  const defaults: AppConfig = {
    serverPort: envServerPort || 3001,
    webPort: envWebPort || 3000,
    apiEndpoint: envApiEndpoint || `http://localhost:${envServerPort || 3001}`,
    websiteUrl: envWebsiteUrl || `http://localhost:${envWebPort || 3000}`,
    livekitUrl: envLivekitUrl || 'ws://localhost:7880',
    livekitApiKey: envLivekitApiKey || 'devkey',
    livekitApiSecret: envLivekitApiSecret || 'secret',
  };

  const configPath = findConfigFile();
  const source = configPath ? `file (${configPath})` : 'defaults (no config file found)';

  if (configPath) {
    try {
      const fileConfig = JSON.parse(readFileSync(configPath, 'utf-8')) as Partial<AppConfig>;
      const resolved: AppConfig = {
        serverPort: envServerPort || fileConfig.serverPort || defaults.serverPort,
        webPort: envWebPort || fileConfig.webPort || defaults.webPort,
        apiEndpoint:
          envApiEndpoint ||
          fileConfig.apiEndpoint ||
          `http://localhost:${envServerPort || fileConfig.serverPort || 3001}`,
        websiteUrl:
          envWebsiteUrl || fileConfig.websiteUrl || `http://localhost:${envWebPort || fileConfig.webPort || 3000}`,
        livekitUrl: envLivekitUrl || fileConfig.livekitUrl || defaults.livekitUrl,
        livekitApiKey: envLivekitApiKey || fileConfig.livekitApiKey || defaults.livekitApiKey,
        livekitApiSecret: envLivekitApiSecret || fileConfig.livekitApiSecret || defaults.livekitApiSecret,
      };
      console.log(
        `[config] loaded from ${source}: livekitUrl=${resolved.livekitUrl}, apiKey=${resolved.livekitApiKey.slice(0, 8)}..., serverPort=${resolved.serverPort}`,
      );
      return resolved;
    } catch {
      console.error('Failed to parse slopcast.config.json, using env/defaults');
    }
  }

  console.log(
    `[config] using ${source}: livekitUrl=${defaults.livekitUrl}, apiKey=${defaults.livekitApiKey.slice(0, 8)}..., serverPort=${defaults.serverPort}`,
  );
  return defaults;
}
