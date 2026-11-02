import { createHash, randomBytes, timingSafeEqual } from 'node:crypto';

import { ROOM_CODE_RE, isJsonObject, readString } from '@slopcast/shared-types';
import { Router as createRouter } from 'express';
import type { Request, Response, Router } from 'express';
import { RoomServiceClient } from 'livekit-server-sdk';

import { generateRoomCode } from './roomCodes.js';
import { presenterToken, spectatorToken } from './token.js';

// How long a room survives without its presenter in LiveKit and without a presenter heartbeat.
// Long enough to ride out a Wi-Fi handover, a router restart, or a laptop lid close.
export const PRESENTER_GRACE_MS = 120_000;
// LiveKit's own empty/departure timeouts only matter while this API server is down. The sweep is
// The authority on room lifetime, so LiveKit must never close a room before the sweep would.
const LIVEKIT_ROOM_TIMEOUT_S = 600;
const NEXT_INDEX = 1;
const ROOM_SWEEP_MS = 15_000;
const SLOPCAST_OWNER = 'slopcast';

export interface ActiveRoom {
  closeKeyHash: string;
  presenterIdentity: string;
  presenterMissingSince: number | null;
  lastHeartbeatAt: number | null;
}

// Room state lives in LiveKit room metadata so an API server restart adopts live rooms instead
// Of orphaning them. Only a hash of the close key is stored: metadata is visible to participants.
interface PersistedRoom {
  closeKeyHash: string;
  presenterIdentity: string;
}

export interface RoomClient {
  readonly createRoom: (options: {
    readonly name: string;
    readonly emptyTimeout?: number;
    readonly departureTimeout?: number;
    readonly metadata?: string;
  }) => Promise<{ name: string; metadata: string }>;
  readonly deleteRoom: (roomName: string) => Promise<void>;
  readonly listParticipants: (roomName: string) => Promise<{ identity: string }[]>;
  readonly listRooms: (names?: string[]) => Promise<{ name: string; metadata: string }[]>;
}

export const toWsUrl = (url: string): string => {
  if (url.startsWith('ws://') || url.startsWith('wss://')) {
    return url;
  }
  return url.replace(/^http(?<secure>s?):\/\//u, 'ws$<secure>://');
};

export const toHttpUrl = (url: string): string => {
  let normalized = url.replace(/^ws(?<secure>s?):\/\//u, 'http$<secure>://');
  try {
    const parsed = new URL(normalized);
    if (parsed.hostname === 'localhost') {
      parsed.hostname = '127.0.0.1';
    }
    normalized = parsed.toString().replace(/\/$/u, '');
  } catch (error) {
    console.debug('[routes] invalid URL:', error);
  }
  return normalized;
};

export const countSpectators = (participants: readonly { identity: string }[]): number =>
  participants.filter((participant) => participant.identity.startsWith('spectator-')).length;

export const hashCloseKey = (closeKey: string): string => createHash('sha256').update(closeKey).digest('hex');

const serializeRoomMetadata = (room: PersistedRoom): string =>
  JSON.stringify({
    closeKeyHash: room.closeKeyHash,
    owner: SLOPCAST_OWNER,
    presenterIdentity: room.presenterIdentity,
  });

type ParsedRoomMetadata = { kind: 'foreign' } | { kind: 'legacy' } | { kind: 'slopcast'; room: PersistedRoom };

export const parseRoomMetadata = (metadata: string): ParsedRoomMetadata => {
  let parsed: unknown = undefined;
  try {
    // SAFETY: the shape is validated field by field below.
    parsed = JSON.parse(metadata);
  } catch {
    return { kind: 'foreign' };
  }
  if (!isJsonObject(parsed) || readString(parsed, 'owner') !== SLOPCAST_OWNER) {
    return { kind: 'foreign' };
  }

  const closeKeyHash = readString(parsed, 'closeKeyHash');
  const presenterIdentity = readString(parsed, 'presenterIdentity');
  if (
    closeKeyHash === undefined ||
    !/^[0-9a-f]{64}$/u.test(closeKeyHash) ||
    presenterIdentity === undefined ||
    !presenterIdentity.startsWith('presenter-')
  ) {
    return { kind: 'legacy' };
  }
  return { kind: 'slopcast', room: { closeKeyHash, presenterIdentity } };
};

const closeKeyMatches = (expectedHash: string, provided: string): boolean => {
  const expectedBytes = Buffer.from(expectedHash, 'hex');
  const providedBytes = Buffer.from(hashCloseKey(provided), 'hex');
  if (expectedBytes.length !== providedBytes.length) {
    return false;
  }

  return timingSafeEqual(expectedBytes, providedBytes);
};

const hasRecentHeartbeat = (room: ActiveRoom, now: number): boolean =>
  room.lastHeartbeatAt !== null && now - room.lastHeartbeatAt < PRESENTER_GRACE_MS;

class AllocationError extends Error {
  readonly status: number;

  constructor(message: string, status: number) {
    super(message);
    this.status = status;
  }
}

const deleteRoomIfPresent = async (roomClient: RoomClient, roomName: string): Promise<boolean> => {
  try {
    await roomClient.deleteRoom(roomName);
    return true;
  } catch (error) {
    const matchingRooms = await roomClient.listRooms([roomName]).catch(() => null);
    if (matchingRooms?.length === 0) {
      return true;
    }
    console.error(`Room deletion failed for ${roomName}:`, error);
    return false;
  }
};

const mintPresenterCredentials = async (
  apiKey: string,
  apiSecret: string,
  code: string,
  identity: string,
): Promise<{ token: string; identity: string }> => {
  try {
    const token = await presenterToken(apiKey, apiSecret, code, identity);
    return { identity, token };
  } catch (error) {
    console.error('Token minting failed for room:', error);
    throw new AllocationError('Failed to create room, please try again', 500);
  }
};

export const sweepActiveRoom = async (
  roomClient: RoomClient,
  activeRooms: Map<string, ActiveRoom>,
  code: string,
  room: ActiveRoom,
  now: number,
): Promise<void> => {
  try {
    const participants = await roomClient.listParticipants(code);
    const hasPresenter = participants.some((participant) => participant.identity.startsWith('presenter-'));
    // A presenter that is still heartbeating is reconnecting to LiveKit, not gone.
    if (hasPresenter || hasRecentHeartbeat(room, now)) {
      room.presenterMissingSince = null;
      return;
    }

    room.presenterMissingSince ??= now;
    if (now - room.presenterMissingSince < PRESENTER_GRACE_MS) {
      return;
    }
    if (await deleteRoomIfPresent(roomClient, code)) {
      activeRooms.delete(code);
    }
  } catch (error) {
    const matchingRooms = await roomClient.listRooms([code]).catch(() => null);
    if (matchingRooms?.length === 0) {
      activeRooms.delete(code);
      return;
    }

    console.warn(`Presenter cleanup check failed for ${code}:`, error);
  }
};

const allocateRoomCode = async (roomClient: RoomClient, activeRooms: Map<string, ActiveRoom>): Promise<string> => {
  const livekitRooms = await roomClient.listRooms();
  const livekitNames = new Set(livekitRooms.map((room) => room.name));
  for (let attempts = 0; attempts <= 5; attempts++) {
    const candidate = generateRoomCode();
    if (!activeRooms.has(candidate) && !livekitNames.has(candidate)) {
      return candidate;
    }
  }

  throw new AllocationError('Could not allocate a unique room code, try again', 503);
};

const createActiveRoom = async (
  roomClient: RoomClient,
  activeRooms: Map<string, ActiveRoom>,
  apiKey: string,
  apiSecret: string,
): Promise<{ closeKey: string; code: string; identity: string; token: string }> => {
  const code = await allocateRoomCode(roomClient, activeRooms);
  const closeKey = randomBytes(32).toString('base64url');
  const closeKeyHash = hashCloseKey(closeKey);
  const presenterIdentity = `presenter-${code}-${Date.now()}`;

  await roomClient.createRoom({
    departureTimeout: LIVEKIT_ROOM_TIMEOUT_S,
    emptyTimeout: LIVEKIT_ROOM_TIMEOUT_S,
    metadata: serializeRoomMetadata({ closeKeyHash, presenterIdentity }),
    name: code,
  });
  activeRooms.set(code, {
    closeKeyHash,
    lastHeartbeatAt: null,
    presenterIdentity,
    presenterMissingSince: Date.now(),
  });

  try {
    const credentials = await mintPresenterCredentials(apiKey, apiSecret, code, presenterIdentity);
    return { closeKey, code, ...credentials };
  } catch (error) {
    activeRooms.delete(code);
    await deleteRoomIfPresent(roomClient, code);
    throw error;
  }
};

export const initRoutes = (
  host: string,
  apiKey: string,
  apiSecret: string,
  websiteUrl: string,
  clientUrl?: string,
  injectedRoomClient?: RoomClient,
): Router => {
  const roomClient = injectedRoomClient ?? new RoomServiceClient(toHttpUrl(host), apiKey, apiSecret);
  const livekitWsUrl = toWsUrl(clientUrl ?? host);
  const activeRooms = new Map<string, ActiveRoom>();
  const router = createRouter();

  let isStartupComplete = false;
  let startupPromise: Promise<void> | null = null;
  const ensureStartupCleanup = (): Promise<void> => {
    if (isStartupComplete) {
      return Promise.resolve();
    }
    if (startupPromise) {
      return startupPromise;
    }

    startupPromise = (async (): Promise<void> => {
      // Adopt rooms that a previous server process created so a restart never interrupts a
      // Live stream. Rooms whose presenter is really gone are reaped by the regular sweep.
      const rooms = await roomClient.listRooms();
      const legacyRooms: string[] = [];
      for (const room of rooms) {
        const parsed = parseRoomMetadata(room.metadata);
        if (parsed.kind === 'slopcast' && ROOM_CODE_RE.test(room.name)) {
          if (!activeRooms.has(room.name)) {
            activeRooms.set(room.name, { ...parsed.room, lastHeartbeatAt: null, presenterMissingSince: null });
          }
        } else if (parsed.kind !== 'foreign') {
          // Tagged rooms without a persisted close key cannot be closed or refreshed by anyone.
          legacyRooms.push(room.name);
        }
      }
      const results = await Promise.allSettled(legacyRooms.map((name) => roomClient.deleteRoom(name)));
      const failure = results.find((result) => result.status === 'rejected');
      if (failure?.status === 'rejected') {
        throw failure.reason;
      }
      isStartupComplete = true;
    })().finally(() => {
      startupPromise = null;
    });

    return startupPromise;
  };

  let isSweeping = false;
  const sweepMissingPresenters = async (): Promise<void> => {
    if (isSweeping) {
      return;
    }
    isSweeping = true;

    try {
      await ensureStartupCleanup();
      const now = Date.now();
      const entries = [...activeRooms.entries()];
      const sweepFrom = async (index: number): Promise<void> => {
        const entry = entries[index];
        if (entry === undefined) {
          return;
        }
        await sweepActiveRoom(roomClient, activeRooms, entry[0], entry[1], now);
        await sweepFrom(index + NEXT_INDEX);
      };
      await sweepFrom(0);
    } catch (error) {
      console.error('Room cleanup sweep failed:', error);
    } finally {
      isSweeping = false;
    }
  };

  // Until previous rooms are adopted, "not found" would be a lie that makes clients give up.
  const awaitStartup = async (response: Response): Promise<boolean> => {
    try {
      await ensureStartupCleanup();
      return true;
    } catch (error) {
      console.error('Room recovery is not complete:', error);
      response.status(503).json({ error: 'Streaming service is temporarily unavailable' });
      return false;
    }
  };

  const sweepTimer = setInterval(() => void sweepMissingPresenters(), ROOM_SWEEP_MS);
  sweepTimer.unref();

  const health = async (_request: Request, response: Response) => {
    try {
      await ensureStartupCleanup();
      const rooms = await roomClient.listRooms();
      response.json({ activeRooms: rooms.length, status: 'ok' });
    } catch (error) {
      console.error('Health check failed:', error);
      response.status(503).json({ activeRooms: 0, error: 'LiveKit unreachable', status: 'degraded' });
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
        livekitUrl: livekitWsUrl,
        shareUrl: `${websiteUrl}/room/${room.code}`,
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

    if (!(await awaitStartup(res))) {
      return;
    }
    const room = activeRooms.get(code);
    if (!room) {
      res.status(404).json({ error: 'Room not found' });
      return;
    }

    const closeKey = req.get('X-Room-Close-Key');
    if (closeKey === undefined || closeKey === '' || !closeKeyMatches(room.closeKeyHash, closeKey)) {
      res.status(403).json({ error: 'Invalid room close key' });
      return;
    }

    if (!(await deleteRoomIfPresent(roomClient, code))) {
      res.status(503).json({ error: 'Room could not be closed, please try again' });
      return;
    }

    activeRooms.delete(code);
    res.status(204).end();
  });

  // Presenter heartbeat. Proves the presenter app is alive (keeping the room through LiveKit
  // Outages) and hands back a fresh token so reconnects keep working past the original TTL.
  router.post('/api/rooms/:code/presenter-token', async (req, res) => {
    const { code } = req.params;
    if (!ROOM_CODE_RE.test(code)) {
      res.status(400).json({ error: 'Invalid room code format' });
      return;
    }
    if (!(await awaitStartup(res))) {
      return;
    }
    const room = activeRooms.get(code);
    if (!room) {
      res.status(404).json({ error: 'Room not found or closed' });
      return;
    }

    const closeKey = req.get('X-Room-Close-Key');
    if (closeKey === undefined || closeKey === '' || !closeKeyMatches(room.closeKeyHash, closeKey)) {
      res.status(403).json({ error: 'Invalid room close key' });
      return;
    }

    room.lastHeartbeatAt = Date.now();
    room.presenterMissingSince = null;
    const token = await presenterToken(apiKey, apiSecret, code, room.presenterIdentity);
    res.json({ identity: room.presenterIdentity, livekitUrl: livekitWsUrl, token });
  });

  router.get('/api/rooms/:code/token', async (req, res) => {
    const { code } = req.params;
    if (!ROOM_CODE_RE.test(code)) {
      res.status(400).json({ error: 'Invalid room code format' });
      return;
    }
    if (!(await awaitStartup(res))) {
      return;
    }
    if (!activeRooms.has(code)) {
      res.status(404).json({ error: 'Room not found or closed' });
      return;
    }

    const identity = `spectator-${code}-${Date.now()}`;
    const token = await spectatorToken(apiKey, apiSecret, code, identity);
    res.json({ identity, livekitUrl: livekitWsUrl, token });
  });

  router.get('/api/rooms/:code/spectators', async (req, res) => {
    const { code } = req.params;
    if (!ROOM_CODE_RE.test(code)) {
      res.status(400).json({ error: 'Invalid room code format' });
      return;
    }
    if (!(await awaitStartup(res))) {
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
};
