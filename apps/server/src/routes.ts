import { ROOM_CODE_RE } from '@slopcast/shared-types';
import { Router as createRouter, type Router } from 'express';
import { RoomServiceClient } from 'livekit-server-sdk';

import { generateRoomCode } from './roomCodes.js';
import { presenterToken, spectatorToken } from './token.js';

function toWsUrl(url: string): string {
  if (url.startsWith('ws://') || url.startsWith('wss://')) return url;
  return url.replace(/^http(s?):\/\//, 'ws$1://');
}

function toHttpUrl(url: string): string {
  let normalized = url.replace(/^ws(s?):\/\//, 'http$1://');
  try {
    const parsed = new URL(normalized);
    if (parsed.hostname === 'localhost') {
      parsed.hostname = '127.0.0.1';
    }
    normalized = parsed.toString().replace(/\/$/, '');
  } catch {
    // Ignore invalid URLs
  }
  return normalized;
}

export function initRoutes(
  host: string,
  apiKey: string,
  apiSecret: string,
  websiteUrl: string,
  clientUrl?: string,
): Router {
  const roomClient = new RoomServiceClient(toHttpUrl(host), apiKey, apiSecret);
  const livekitWsUrl = toWsUrl(clientUrl ?? host);

  const router = createRouter();

  const allocatedCodes = new Map<string, number>();
  const ROOM_CODE_TTL_MS = 24 * 60 * 60 * 1000;

  const sweepExpiredCodes = () => {
    const now = Date.now();
    for (const [code, createdAt] of allocatedCodes.entries()) {
      if (now - createdAt > ROOM_CODE_TTL_MS) {
        allocatedCodes.delete(code);
      }
    }
  };

  const health = async (
    _req: unknown,
    res: { json: (o: object) => void; status: (code: number) => { json: (o: object) => void } },
  ) => {
    try {
      const rooms = await roomClient.listRooms();
      res.json({ status: 'ok', activeRooms: rooms.length });
    } catch (err) {
      console.error('Health check failed:', err);
      res.status(503).json({ status: 'degraded', activeRooms: 0, error: 'LiveKit unreachable' });
    }
  };

  router.get('/health', health);
  router.get('/api/health', health);

  router.post('/api/rooms', async (req, res) => {
    const origin = req.headers['x-client-origin'];
    if (origin !== 'desktop') {
      res.status(403).json({ error: 'Only desktop clients can create rooms' });
      return;
    }

    sweepExpiredCodes();

    let code: string;
    do {
      code = generateRoomCode();
    } while (allocatedCodes.has(code));
    allocatedCodes.set(code, Date.now());

    try {
      const active = await roomClient.listRooms();
      const names = new Set(active.map((r) => r.name));
      for (let attempts = 0; names.has(code) && attempts < 5; attempts++) {
        allocatedCodes.delete(code);
        do {
          code = generateRoomCode();
        } while (allocatedCodes.has(code));
        allocatedCodes.set(code, Date.now());
      }
      if (names.has(code)) {
        allocatedCodes.delete(code);
        res.status(503).json({ error: 'Could not allocate a unique room code, try again' });
        return;
      }
    } catch (err) {
      allocatedCodes.delete(code);
      console.error('Room creation failed, SFU unreachable:', err);
      res.status(503).json({ error: 'Streaming service is temporarily unavailable, please try again' });
      return;
    }

    const identity = `presenter-${code}-${Date.now()}`;
    const nativeIdentity = `audio-${code}-${Date.now()}`;
    let token: string;
    let nativeToken: string;
    try {
      token = await presenterToken(apiKey, apiSecret, code, identity);
      nativeToken = await presenterToken(apiKey, apiSecret, code, nativeIdentity);
    } catch (err) {
      allocatedCodes.delete(code);
      console.error('Token minting failed for room code, code not allocated:', err);
      res.status(500).json({ error: 'Failed to create room, please try again' });
      return;
    }

    res.json({
      code,
      shareUrl: `${websiteUrl}/room/${code}`,
      token,
      identity,
      nativeToken,
      nativeIdentity,
      livekitUrl: livekitWsUrl,
    });
  });

  router.get('/api/rooms/:code/token', async (req, res) => {
    const { code } = req.params;
    if (!ROOM_CODE_RE.test(code)) {
      res.status(400).json({ error: 'Invalid room code format' });
      return;
    }
    const identity = `spectator-${code}-${Date.now()}`;
    const token = await spectatorToken(apiKey, apiSecret, code, identity);

    res.json({ token, identity, livekitUrl: livekitWsUrl });
  });

  return router;
}
