export interface AppConfig {
  serverPort: number;
  webPort: number;
  apiEndpoint: string;
  websiteUrl: string;
  livekitUrl: string;
  livekitApiKey: string;
  livekitApiSecret: string;
}

export type ClientRole = 'presenter' | 'spectator';
export type ClientOrigin = 'desktop' | 'web';

export interface Participant {
  id: string;
  role: ClientRole;
  origin: ClientOrigin;
  joinedAt: number;
}

export interface ErrorPayload {
  message: string;
  code?: string;
}
