import { Router as createRouter, type Router } from 'express';
import { RoomServiceClient } from 'livekit-server-sdk';

import { generateRoomCode } from './roomCodes.js';
import { presenterToken, spectatorToken } from './token.js';

export function initRoutes(host: string, apiKey: string, apiSecret: string, websiteUrl: string): Router {
  const roomClient = new RoomServiceClient(host, apiKey, apiSecret);

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
    const origin = req.headers['x-client-origin'] ?? 'desktop';
    if (origin === 'web') {
      res.status(403).json({ error: 'Web clients cannot create rooms' });
      return;
    }

    const code = generateRoomCode();
    const identity = `presenter-${code}-${Date.now()}`;
    const token = await presenterToken(apiKey, apiSecret, code, identity);

    res.json({ code, shareUrl: `${websiteUrl}/room/${code}`, token, identity, livekitUrl: host });
  });

  router.get('/api/rooms/:code/token', async (req, res) => {
    const { code } = req.params;
    const identity = `spectator-${code}-${Date.now()}`;
    const token = await spectatorToken(apiKey, apiSecret, code, identity);

    res.json({ token, identity, livekitUrl: host });
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
