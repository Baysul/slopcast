import { randomBytes, timingSafeEqual } from 'node:crypto';
import { ROOM_CODE_RE } from '@slopcast/shared-types';
import { Router as createRouter, type Request, type Response, type Router } from 'express';
import { RoomServiceClient } from 'livekit-server-sdk';

import { generateRoomCode } from './roomCodes.js';
import { presenterToken, spectatorToken } from './token.js';

const PRESENTER_GRACE_MS = 60_000;
const ROOM_SWEEP_MS = 15_000;
const SLOPCAST_ROOM_METADATA = JSON.stringify({ owner: 'slopcast' });

interface ActiveRoom {
  closeKey: string;
  presenterMissingSince: number | null;
}

export interface RoomClient {
  createRoom(options: {
    name: string;
    emptyTimeout?: number;
    departureTimeout?: number;
    metadata?: string;
  }): Promise<{ name: string; metadata: string }>;
  deleteRoom(roomName: string): Promise<void>;
  listParticipants(roomName: string): Promise<Array<{ identity: string }>>;
  listRooms(names?: string[]): Promise<Array<{ name: string; metadata: string }>>;
}

export function toWsUrl(url: string): string {
  if (url.startsWith('ws://') || url.startsWith('wss://')) return url;
  return url.replace(/^http(s?):\/\//, 'ws$1://');
}

export function toHttpUrl(url: string): string {
  let normalized = url.replace(/^ws(s?):\/\//, 'http$1://');
  try {
    const parsed = new URL(normalized);
    if (parsed.hostname === 'localhost') {
      parsed.hostname = '127.0.0.1';
    }
    normalized = parsed.toString().replace(/\/$/, '');
  } catch (error) {
    console.debug('[routes] invalid URL:', error);
  }
  return normalized;
}

export function countSpectators(participants: ReadonlyArray<{ identity: string }>): number {
  return participants.filter((participant) => participant.identity.startsWith('spectator-')).length;
}

function isSlopcastRoom(metadata: string): boolean {
  try {
    // SAFETY: Slopcast creates this room metadata in createActiveRoom.
    const parsed = JSON.parse(metadata) as { owner?: string };
    return parsed.owner === 'slopcast';
  } catch {
    return false;
  }
}

function closeKeyMatches(expected: string, provided: string): boolean {
  const expectedBytes = Buffer.from(expected);
  const providedBytes = Buffer.from(provided);
  if (expectedBytes.length !== providedBytes.length) return false;

  return timingSafeEqual(expectedBytes, providedBytes);
}

class AllocationError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
  }
}

async function deleteRoomIfPresent(roomClient: RoomClient, roomName: string): Promise<boolean> {
  try {
    await roomClient.deleteRoom(roomName);
    return true;
  } catch (error) {
    const matchingRooms = await roomClient.listRooms([roomName]).catch(() => null);
    if (matchingRooms?.length === 0) return true;
    console.error(`Room deletion failed for ${roomName}:`, error);
    return false;
  }
}

async function mintPresenterCredentials(
  apiKey: string,
  apiSecret: string,
  code: string,
): Promise<{ token: string; identity: string }> {
  const identity = `presenter-${code}-${Date.now()}`;
  try {
    const token = await presenterToken(apiKey, apiSecret, code, identity);
    return { token, identity };
  } catch (error) {
    console.error('Token minting failed for room:', error);
    throw new AllocationError('Failed to create room, please try again', 500);
  }
}

async function sweepActiveRoom(
  roomClient: RoomClient,
  activeRooms: Map<string, ActiveRoom>,
  code: string,
  room: ActiveRoom,
  now: number,
): Promise<void> {
  try {
    const participants = await roomClient.listParticipants(code);
    const hasPresenter = participants.some((participant) => participant.identity.startsWith('presenter-'));
    if (hasPresenter) {
      room.presenterMissingSince = null;
      return;
    }

    room.presenterMissingSince ??= now;
    if (now - room.presenterMissingSince < PRESENTER_GRACE_MS) return;
    if (await deleteRoomIfPresent(roomClient, code)) activeRooms.delete(code);
  } catch (error) {
    const matchingRooms = await roomClient.listRooms([code]).catch(() => null);
    if (matchingRooms?.length === 0) {
      activeRooms.delete(code);
      return;
    }

    console.warn(`Presenter cleanup check failed for ${code}:`, error);
  }
}

async function allocateRoomCode(roomClient: RoomClient, activeRooms: Map<string, ActiveRoom>): Promise<string> {
  const livekitRooms = await roomClient.listRooms();
  const livekitNames = new Set(livekitRooms.map((room) => room.name));
  for (let attempts = 0; attempts <= 5; attempts++) {
    const candidate = generateRoomCode();
    if (!activeRooms.has(candidate) && !livekitNames.has(candidate)) return candidate;
  }

  throw new AllocationError('Could not allocate a unique room code, try again', 503);
}

async function createActiveRoom(
  roomClient: RoomClient,
  activeRooms: Map<string, ActiveRoom>,
  apiKey: string,
  apiSecret: string,
): Promise<{ closeKey: string; code: string; identity: string; token: string }> {
  const code = await allocateRoomCode(roomClient, activeRooms);
  const closeKey = randomBytes(32).toString('base64url');

  await roomClient.createRoom({
    name: code,
    emptyTimeout: PRESENTER_GRACE_MS / 1000,
    departureTimeout: PRESENTER_GRACE_MS / 1000,
    metadata: SLOPCAST_ROOM_METADATA,
  });
  activeRooms.set(code, { closeKey, presenterMissingSince: Date.now() });

  try {
    const credentials = await mintPresenterCredentials(apiKey, apiSecret, code);
    return { code, closeKey, ...credentials };
  } catch (error) {
    activeRooms.delete(code);
    await deleteRoomIfPresent(roomClient, code);
    throw error;
  }
}

export function initRoutes(
  host: string,
  apiKey: string,
  apiSecret: string,
  websiteUrl: string,
  clientUrl?: string,
  injectedRoomClient?: RoomClient,
): Router {
  const roomClient = injectedRoomClient ?? new RoomServiceClient(toHttpUrl(host), apiKey, apiSecret);
  const livekitWsUrl = toWsUrl(clientUrl ?? host);
  const activeRooms = new Map<string, ActiveRoom>();
  const router = createRouter();

  let isStartupComplete = false;
  let startupPromise: Promise<void> | null = null;
  const ensureStartupCleanup = (): Promise<void> => {
    if (isStartupComplete) return Promise.resolve();
    if (startupPromise) return startupPromise;

    startupPromise = (async (): Promise<void> => {
      const rooms = await roomClient.listRooms();
      const taggedRooms = rooms.filter((room) => isSlopcastRoom(room.metadata));
      const results = await Promise.allSettled(taggedRooms.map((room) => roomClient.deleteRoom(room.name)));
      const failure = results.find((result) => result.status === 'rejected');
      if (failure?.status === 'rejected') throw failure.reason;
      isStartupComplete = true;
    })().finally(() => {
      startupPromise = null;
    });

    return startupPromise;
  };

  let isSweeping = false;
  const sweepMissingPresenters = async (): Promise<void> => {
    if (isSweeping) return;
    isSweeping = true;

    try {
      await ensureStartupCleanup();
      const now = Date.now();
      for (const [code, room] of activeRooms) {
        await sweepActiveRoom(roomClient, activeRooms, code, room, now);
      }
    } catch (error) {
      console.error('Room cleanup sweep failed:', error);
    } finally {
      isSweeping = false;
    }
  };

  const sweepTimer = setInterval(() => void sweepMissingPresenters(), ROOM_SWEEP_MS);
  sweepTimer.unref();

  const health = async (_request: Request, response: Response) => {
    try {
      await ensureStartupCleanup();
      const rooms = await roomClient.listRooms();
      response.json({ status: 'ok', activeRooms: rooms.length });
    } catch (error) {
      console.error('Health check failed:', error);
      response.status(503).json({ status: 'degraded', activeRooms: 0, error: 'LiveKit unreachable' });
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

    try {
      await ensureStartupCleanup();
      const room = await createActiveRoom(roomClient, activeRooms, apiKey, apiSecret);
      res.json({
        ...room,
        shareUrl: `${websiteUrl}/room/${room.code}`,
        livekitUrl: livekitWsUrl,
      });
    } catch (error) {
      if (!(error instanceof AllocationError)) {
        console.error('Room creation failed:', error);
      }
      res.status(error instanceof AllocationError ? error.status : 503).json({
        error: error instanceof AllocationError ? error.message : 'Streaming service is temporarily unavailable',
      });
    }
  });

  router.delete('/api/rooms/:code', async (req, res) => {
    const { code } = req.params;
    if (!ROOM_CODE_RE.test(code)) {
      res.status(400).json({ error: 'Invalid room code format' });
      return;
    }

    const room = activeRooms.get(code);
    if (!room) {
      res.status(404).json({ error: 'Room not found' });
      return;
    }

    const closeKey = req.get('X-Room-Close-Key');
    if (!closeKey || !closeKeyMatches(room.closeKey, closeKey)) {
      res.status(403).json({ error: 'Invalid room close key' });
      return;
    }

    await ensureStartupCleanup();
    if (!(await deleteRoomIfPresent(roomClient, code))) {
      res.status(503).json({ error: 'Room could not be closed, please try again' });
      return;
    }

    activeRooms.delete(code);
    res.status(204).end();
  });

  router.get('/api/rooms/:code/token', async (req, res) => {
    const { code } = req.params;
    if (!ROOM_CODE_RE.test(code)) {
      res.status(400).json({ error: 'Invalid room code format' });
      return;
    }
    if (!activeRooms.has(code)) {
      res.status(404).json({ error: 'Room not found or closed' });
      return;
    }

    const identity = `spectator-${code}-${Date.now()}`;
    const token = await spectatorToken(apiKey, apiSecret, code, identity);
    res.json({ token, identity, livekitUrl: livekitWsUrl });
  });

  router.get('/api/rooms/:code/spectators', async (req, res) => {
    const { code } = req.params;
    if (!ROOM_CODE_RE.test(code)) {
      res.status(400).json({ error: 'Invalid room code format' });
      return;
    }
    if (!activeRooms.has(code)) {
      res.status(404).json({ error: 'Room not found or closed' });
      return;
    }

    try {
      const participants = await roomClient.listParticipants(code);
      res.json({ count: countSpectators(participants) });
    } catch (error) {
      console.error('Spectator count failed:', error);
      res.status(503).json({ error: 'Spectator count temporarily unavailable' });
    }
  });

  return router;
}
