import { Router as createRouter, type Router } from 'express';
import { RoomServiceClient } from 'livekit-server-sdk';

import { generateRoomCode } from './roomCodes.js';
import { presenterToken, spectatorToken } from './token.js';

function toWsUrl(url: string): string {
  if (url.startsWith('ws://') || url.startsWith('wss://')) return url;
  return url.replace(/^http(s?):\/\//, 'ws$1://');
}

export function initRoutes(host: string, apiKey: string, apiSecret: string, websiteUrl: string): Router {
  const roomClient = new RoomServiceClient(host, apiKey, apiSecret);

  const livekitWsUrl = toWsUrl(host);

  const router = createRouter();

  const health = async (_req: unknown, res: { json: (o: object) => void }) => {
    let activeRooms = -1;
    try {
      const rooms = await roomClient.listRooms();
      activeRooms = rooms.length;
    } catch (err) {
      console.error('Health check failed:', err);
    }
    res.json({ status: 'ok', activeRooms });
  };

  router.get('/health', health);

  router.post('/api/rooms', async (req, res) => {
    // Soft enforcement: the desktop app declares itself via this header. The
    // hard publish barrier is the SFU — spectator tokens get canPublish:false.
    const origin = req.headers['x-client-origin'];
    if (origin !== 'desktop') {
      res.status(403).json({ error: 'Only desktop clients can create rooms' });
      return;
    }

    // Room names live in LiveKit; regenerate on the (astronomically unlikely)
    // collision with an active room.
    let code = generateRoomCode();
    try {
      const active = await roomClient.listRooms();
      const names = new Set(active.map((r) => r.name));
      for (let attempts = 0; names.has(code) && attempts < 5; attempts++) {
        code = generateRoomCode();
      }
      if (names.has(code)) {
        res.status(503).json({ error: 'Could not allocate a unique room code, try again' });
        return;
      }
    } catch (err) {
      // The SFU being unreachable must not block room creation.
      console.error('Room collision check failed, proceeding unchecked:', err);
    }

    const identity = `presenter-${code}-${Date.now()}`;
    const token = await presenterToken(apiKey, apiSecret, code, identity);

    res.json({ code, shareUrl: `${websiteUrl}/room/${code}`, token, identity, livekitUrl: livekitWsUrl });
  });

  router.get('/api/rooms/:code/token', async (req, res) => {
    const { code } = req.params;
    const identity = `spectator-${code}-${Date.now()}`;
    const token = await spectatorToken(apiKey, apiSecret, code, identity);

    res.json({ token, identity, livekitUrl: livekitWsUrl });
  });

  router.get('/api/rooms/:code', async (req, res) => {
    const { code } = req.params;
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
