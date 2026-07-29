import { Router as createRouter, type Router } from 'express';
import { RoomServiceClient } from 'livekit-server-sdk';

import { generateRoomCode } from './roomCodes.js';
import { presenterToken, spectatorToken } from './token.js';

const ROOM_CODE_RE = /^[a-z]{3}-[0-9]{3}-[a-z]{3}$/;

function toWsUrl(url: string): string {
  if (url.startsWith('ws://') || url.startsWith('wss://')) return url;
  return url.replace(/^http(s?):\/\//, 'ws$1://');
}

export function initRoutes(host: string, apiKey: string, apiSecret: string, websiteUrl: string): Router {
  const roomClient = new RoomServiceClient(host, apiKey, apiSecret);
  const livekitWsUrl = toWsUrl(host);

  const router = createRouter();

  const allocatedCodes = new Set<string>();

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

  router.post('/api/rooms', async (req, res) => {
    const origin = req.headers['x-client-origin'];
    if (origin !== 'desktop') {
      res.status(403).json({ error: 'Only desktop clients can create rooms' });
      return;
    }

    let code: string;
    do {
      code = generateRoomCode();
    } while (allocatedCodes.has(code));
    allocatedCodes.add(code);

    try {
      const active = await roomClient.listRooms();
      const names = new Set(active.map((r) => r.name));
      for (let attempts = 0; names.has(code) && attempts < 5; attempts++) {
        allocatedCodes.delete(code);
        do {
          code = generateRoomCode();
        } while (allocatedCodes.has(code));
        allocatedCodes.add(code);
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
    const token = await presenterToken(apiKey, apiSecret, code, identity);
    const nativeToken = await presenterToken(apiKey, apiSecret, code, nativeIdentity);

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

  router.get('/api/rooms/:code', async (req, res) => {
    const { code } = req.params;
    if (!ROOM_CODE_RE.test(code)) {
      res.status(400).json({ error: 'Invalid room code format' });
      return;
    }
    try {
      const participants = await roomClient.listParticipants(code);
      res.json({
        code,
        participantCount: participants.length,
        participants: participants.map((p) => ({
          id: p.identity,
          name: p.name,
          isPublisher: p.isPublisher,
        })),
      });
    } catch (err) {
      console.error(`Room lookup failed for ${code}:`, err);
      res.status(404).json({ error: 'Room not found' });
    }
  });

  return router;
}
