import { AccessToken } from 'livekit-server-sdk';

export function presenterToken(apiKey: string, apiSecret: string, room: string, identity: string): Promise<string> {
  const at = new AccessToken(apiKey, apiSecret, { identity, ttl: '6h' });
  at.addGrant({ roomJoin: true, room, canPublish: true, canSubscribe: true, canPublishData: true });
  return at.toJwt();
}

export function spectatorToken(apiKey: string, apiSecret: string, room: string, identity: string): Promise<string> {
  const at = new AccessToken(apiKey, apiSecret, { identity, ttl: '6h' });
  at.addGrant({ roomJoin: true, room, canPublish: false, canSubscribe: true, canPublishData: false });
  return at.toJwt();
}
