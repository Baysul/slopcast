import { AccessToken } from 'livekit-server-sdk';

export function presenterToken(apiKey: string, apiSecret: string, room: string, identity: string): Promise<string> {
  const at = new AccessToken(apiKey, apiSecret, { identity, ttl: '6h' });
  // Linux's direct publisher never subscribes, and no presenter path uses
  // LiveKit data packets. Keep the grant least-privileged on every platform.
  at.addGrant({ roomJoin: true, room, canPublish: true, canSubscribe: false, canPublishData: false });
  return at.toJwt();
}

export function spectatorToken(apiKey: string, apiSecret: string, room: string, identity: string): Promise<string> {
  // INTENTIONAL: spectator tokens are issued without authentication. This is
  // safe under a three-layer defence-in-depth model:
  //
  // 1. HARD BARRIER (SFU): spectator tokens carry canPublish: false and
  //    canPublishData: false — LiveKit's SFU cryptographically enforces these
  //    JWT grants and rejects any publish attempt regardless of token source.
  // 2. SOFT BARRIER (header): POST /api/rooms requires the X-Client-Origin:
  //    desktop header (spoofable via curl, but keeps honest web clients from
  //    minting presenter tokens).
  // 3. RATE LIMITING: the server applies per-IP rate limits to prevent brute-
  //    force room-code guessing. Room codes are CSPRNG-generated (~38 bits of
  //    entropy) and unguessable within the rate-limit window without the
  //    share link.
  //
  // This trade-off prioritises frictionless web-spectator join (no auth) while
  // relying on the SFU's unconditional publish enforcement as the chokepoint.
  const at = new AccessToken(apiKey, apiSecret, { identity, ttl: '6h' });
  at.addGrant({ roomJoin: true, room, canPublish: false, canSubscribe: true, canPublishData: false });
  return at.toJwt();
}
