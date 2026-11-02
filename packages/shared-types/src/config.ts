import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';

import type { AppConfig } from './index.js';
import { isJsonObject, readFiniteNumber, readString } from './json.js';

export type { AppConfig };

const findConfigFile = (): string | null => {
  let dir = path.resolve(process.cwd());
  const { root } = path.parse(dir);
  for (;;) {
    const candidate = path.join(dir, 'slopcast.config.json');
    if (existsSync(candidate)) {
      return candidate;
    }
    if (dir === root) {
      break;
    }
    dir = path.dirname(dir);
  }
  return null;
};

interface EnvConfig {
  serverPort: number;
  webPort: number;
  apiEndpoint: string;
  websiteUrl: string;
  livekitUrl: string;
  livekitApiKey: string;
  livekitApiSecret: string;
}

const nonEmptyString = (value: string | undefined): string | undefined => {
  if (value === '') {
    return undefined;
  }
  return value;
};

const readEnv = (): EnvConfig => ({
  apiEndpoint: process.env.API_ENDPOINT ?? '',
  livekitApiKey: process.env.LIVEKIT_API_KEY ?? '',
  livekitApiSecret: process.env.LIVEKIT_API_SECRET ?? '',
  livekitUrl: process.env.LIVEKIT_URL ?? '',
  serverPort: parseInt(nonEmptyString(process.env.SERVER_PORT) ?? process.env.PORT ?? '', 10) || 0,
  webPort: parseInt(process.env.WEB_PORT ?? '', 10) || 0,
  websiteUrl: process.env.WEBSITE_URL ?? '',
});

const buildDefaults = (env: EnvConfig): AppConfig => ({
  apiEndpoint: env.apiEndpoint || `http://localhost:${env.serverPort || 3001}`,
  livekitApiKey: env.livekitApiKey || 'devkey',
  livekitApiSecret: env.livekitApiSecret || 'secret',
  livekitUrl: env.livekitUrl || 'ws://localhost:7880',
  serverPort: env.serverPort || 3001,
  webPort: env.webPort || 3000,
  websiteUrl: env.websiteUrl || `http://localhost:${env.webPort || 3000}`,
});

const mergeConfig = (env: EnvConfig, defaults: AppConfig, rawFileConfig: unknown): AppConfig => {
  const fileConfig = isJsonObject(rawFileConfig) ? rawFileConfig : {};
  const fileString = (key: string): string => readString(fileConfig, key) ?? '';
  const filePort = (key: string): number => readFiniteNumber(fileConfig, key) ?? 0;
  const serverPort = env.serverPort || filePort('serverPort') || defaults.serverPort;

  return {
    apiEndpoint: env.apiEndpoint || fileString('apiEndpoint') || `http://localhost:${serverPort}`,
    livekitApiKey: env.livekitApiKey || fileString('livekitApiKey') || defaults.livekitApiKey,
    livekitApiSecret: env.livekitApiSecret || fileString('livekitApiSecret') || defaults.livekitApiSecret,
    livekitUrl: env.livekitUrl || fileString('livekitUrl') || defaults.livekitUrl,
    serverPort,
    webPort: env.webPort || filePort('webPort') || defaults.webPort,
    websiteUrl:
      env.websiteUrl || fileString('websiteUrl') || `http://localhost:${env.webPort || filePort('webPort') || 3000}`,
  };
};

const assertProductionConfig = (config: AppConfig): void => {
  if (process.env.NODE_ENV !== 'production' || process.env.ALLOW_DEV_KEYS === 'true') {
    return;
  }
  if (config.livekitApiKey === 'devkey' || config.livekitApiSecret === 'secret') {
    throw new Error('Production environment requires custom LIVEKIT_API_KEY and LIVEKIT_API_SECRET');
  }
};

export const loadConfig = (): AppConfig => {
  const env = readEnv();
  const defaults = buildDefaults(env);

  let config = defaults;
  const configPath = findConfigFile();

  if (configPath !== null) {
    try {
      // SAFETY: mergeConfig validates every optional config field against environment-backed defaults.
      const fileConfig: unknown = JSON.parse(readFileSync(configPath, 'utf-8'));
      config = mergeConfig(env, defaults, fileConfig);
    } catch {
      console.error('Failed to parse slopcast.config.json, using env/defaults');
    }
  }

  assertProductionConfig(config);

  return config;
};
