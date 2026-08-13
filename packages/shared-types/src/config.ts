import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';
import type { AppConfig } from './index.js';

export type { AppConfig };

function findConfigFile(): string | null {
  let dir = path.resolve(process.cwd());
  const root = path.parse(dir).root;
  for (;;) {
    const candidate = path.join(dir, 'slopcast.config.json');
    if (existsSync(candidate)) return candidate;
    if (dir === root) break;
    dir = path.dirname(dir);
  }
  return null;
}

interface EnvConfig {
  serverPort: number;
  webPort: number;
  apiEndpoint: string;
  websiteUrl: string;
  livekitUrl: string;
  livekitApiKey: string;
  livekitApiSecret: string;
}

const readEnv = (): EnvConfig => ({
  serverPort: parseInt(process.env.SERVER_PORT || process.env.PORT || '', 10) || 0,
  webPort: parseInt(process.env.WEB_PORT || '', 10) || 0,
  apiEndpoint: process.env.API_ENDPOINT || '',
  websiteUrl: process.env.WEBSITE_URL || '',
  livekitUrl: process.env.LIVEKIT_URL || '',
  livekitApiKey: process.env.LIVEKIT_API_KEY || '',
  livekitApiSecret: process.env.LIVEKIT_API_SECRET || '',
});

const buildDefaults = (env: EnvConfig): AppConfig => ({
  serverPort: env.serverPort || 3001,
  webPort: env.webPort || 3000,
  apiEndpoint: env.apiEndpoint || `http://localhost:${env.serverPort || 3001}`,
  websiteUrl: env.websiteUrl || `http://localhost:${env.webPort || 3000}`,
  livekitUrl: env.livekitUrl || 'ws://localhost:7880',
  livekitApiKey: env.livekitApiKey || 'devkey',
  livekitApiSecret: env.livekitApiSecret || 'secret',
});

const mergeConfig = (env: EnvConfig, defaults: AppConfig, fileConfig: Partial<AppConfig>): AppConfig => ({
  serverPort: env.serverPort || fileConfig.serverPort || defaults.serverPort,
  webPort: env.webPort || fileConfig.webPort || defaults.webPort,
  apiEndpoint:
    env.apiEndpoint || fileConfig.apiEndpoint || `http://localhost:${env.serverPort || fileConfig.serverPort || 3001}`,
  websiteUrl:
    env.websiteUrl || fileConfig.websiteUrl || `http://localhost:${env.webPort || fileConfig.webPort || 3000}`,
  livekitUrl: env.livekitUrl || fileConfig.livekitUrl || defaults.livekitUrl,
  livekitApiKey: env.livekitApiKey || fileConfig.livekitApiKey || defaults.livekitApiKey,
  livekitApiSecret: env.livekitApiSecret || fileConfig.livekitApiSecret || defaults.livekitApiSecret,
});

const assertProductionConfig = (config: AppConfig): void => {
  if (process.env.NODE_ENV !== 'production' || process.env.ALLOW_DEV_KEYS === 'true') return;
  if (config.livekitApiKey === 'devkey' || config.livekitApiSecret === 'secret') {
    throw new Error('Production environment requires custom LIVEKIT_API_KEY and LIVEKIT_API_SECRET');
  }
};

export function loadConfig(): AppConfig {
  const env = readEnv();
  const defaults = buildDefaults(env);

  let config = defaults;
  const configPath = findConfigFile();

  if (configPath) {
    try {
      const fileConfig = JSON.parse(readFileSync(configPath, 'utf-8')) as Partial<AppConfig>;
      config = mergeConfig(env, defaults, fileConfig);
    } catch {
      console.error('Failed to parse slopcast.config.json, using env/defaults');
    }
  }

  assertProductionConfig(config);

  return config;
}
