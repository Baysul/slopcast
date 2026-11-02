import { AccessToken } from 'livekit-server-sdk';

export const presenterToken = (apiKey: string, apiSecret: string, room: string, identity: string): Promise<string> => {
  const at = new AccessToken(apiKey, apiSecret, { identity, ttl: '6h' });
  at.addGrant({ canPublish: true, canPublishData: false, canSubscribe: false, room, roomJoin: true });
  return at.toJwt();
};

// Spectator tokens are intentionally unauthenticated; LiveKit grants deny publishing.
export const spectatorToken = (apiKey: string, apiSecret: string, room: string, identity: string): Promise<string> => {
  const at = new AccessToken(apiKey, apiSecret, { identity, ttl: '6h' });
  at.addGrant({ canPublish: false, canPublishData: false, canSubscribe: true, room, roomJoin: true });
  return at.toJwt();
};
