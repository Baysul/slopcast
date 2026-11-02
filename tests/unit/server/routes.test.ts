import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import type { Server } from 'node:http';

import { isJsonObject, readString } from '@slopcast/shared-types';
import express from 'express';
import { test } from 'vite-plus/test';

import {
  PRESENTER_GRACE_MS,
  hashCloseKey,
  initRoutes,
  parseRoomMetadata,
  sweepActiveRoom,
  toHttpUrl,
  toWsUrl,
} from '../../../apps/server/src/routes.js';
import type { ActiveRoom, RoomClient } from '../../../apps/server/src/routes.js';

test('toWsUrl passes ws/wss through unchanged', () => {
  assert.equal(toWsUrl('ws://localhost:7880'), 'ws://localhost:7880');
  assert.equal(toWsUrl('wss://livekit.example.com'), 'wss://livekit.example.com');
});

test('toWsUrl upgrades http(s) to ws(s)', () => {
  assert.equal(toWsUrl('http://localhost:7880'), 'ws://localhost:7880');
  assert.equal(toWsUrl('https://livekit.example.com'), 'wss://livekit.example.com');
});

test('toHttpUrl downgrades ws(s) to http(s)', () => {
  assert.equal(toHttpUrl('ws://localhost:7880'), 'http://127.0.0.1:7880');
  assert.equal(toHttpUrl('wss://livekit.example.com'), 'https://livekit.example.com');
});

test('toHttpUrl rewrites localhost to 127.0.0.1', () => {
  assert.equal(toHttpUrl('http://localhost:7880'), 'http://127.0.0.1:7880');
  assert.equal(toHttpUrl('http://localhost'), 'http://127.0.0.1');
});

test('toHttpUrl strips a single trailing slash', () => {
  assert.equal(toHttpUrl('http://127.0.0.1:7880/'), 'http://127.0.0.1:7880');
});

class FakeRoomClient implements RoomClient {
  readonly rooms = new Map<string, { name: string; metadata: string }>();
  readonly participants = new Map<string, { identity: string }[]>();
  shouldFailDeletion = false;

  createRoom(options: { name: string; metadata?: string }): Promise<{ name: string; metadata: string }> {
    const room = { metadata: options.metadata ?? '', name: options.name };
    this.rooms.set(room.name, room);
    return Promise.resolve(room);
  }

  deleteRoom(roomName: string): Promise<void> {
    if (this.shouldFailDeletion) {
      return Promise.reject(new Error('deletion unavailable'));
    }
    if (!this.rooms.delete(roomName)) {
      return Promise.reject(new Error('room not found'));
    }
    this.participants.delete(roomName);
    return Promise.resolve();
  }

  listParticipants(roomName: string): Promise<{ identity: string }[]> {
    if (!this.rooms.has(roomName)) {
      return Promise.reject(new Error('room not found'));
    }
    return Promise.resolve(this.participants.get(roomName) ?? []);
  }

  listRooms(names?: string[]): Promise<{ name: string; metadata: string }[]> {
    const rooms = [...this.rooms.values()];
    if (names === undefined || names.length === 0) {
      return Promise.resolve(rooms);
    }
    const requestedNames = new Set(names);
    return Promise.resolve(rooms.filter((room) => requestedNames.has(room.name)));
  }
}

interface TestServer {
  base: string;
  client: FakeRoomClient;
  close: () => Promise<void>;
}

const startTestServer = async (client = new FakeRoomClient()): Promise<TestServer> => {
  const app = express();
  app.use(express.json());
  app.use(initRoutes('ws://localhost:7880', 'devkey', 'secret', 'http://localhost:3000', undefined, client));
  const server: Server = createServer((request, response) => {
    app(request, response);
  });
  await new Promise<void>((resolve) => {
    server.listen(0, '127.0.0.1', resolve);
  });
  const address = server.address();
  if (address === null || typeof address === 'string') {
    throw new Error('test server did not expose a TCP address');
  }

  return {
    base: `http://127.0.0.1:${address.port}`,
    client,
    close: () =>
      new Promise((resolve) => {
        server.closeAllConnections();
        server.close(() => {
          resolve();
        });
      }),
  };
};

const createRoom = async (base: string): Promise<{ closeKey: string; code: string }> => {
  const response = await fetch(`${base}/api/rooms`, {
    headers: { 'Content-Type': 'application/json', 'X-Client-Origin': 'desktop' },
    method: 'POST',
  });
  assert.equal(response.status, 200);
  const body: unknown = await response.json();
  if (!isJsonObject(body)) {
    throw new Error('room response is not a JSON object');
  }
  const code = readString(body, 'code');
  const closeKey = readString(body, 'closeKey');
  assert.ok(code !== undefined);
  assert.ok(closeKey !== undefined);
  assert.match(code, /^[a-z]{3}-\d{3}-[a-z]{3}$/u);
  assert.ok(closeKey.length > 20);
  return { closeKey, code };
};

test('POST /api/rooms without the desktop header is forbidden', async () => {
  const server = await startTestServer();
  try {
    const response = await fetch(`${server.base}/api/rooms`, { method: 'POST' });
    assert.equal(response.status, 403);
    const body: unknown = await response.json();
    if (!isJsonObject(body)) {
      throw new Error('403 response is not a JSON object');
    }
    const message = readString(body, 'error');
    assert.ok(message !== undefined);
    assert.match(message, /desktop clients/iu);
  } finally {
    await server.close();
  }
});

test('spectator token route rejects malformed room codes with 400', async () => {
  const server = await startTestServer();
  try {
    await Promise.all(
      ['ABC-123-XYZ', 'ab-123-xyz', 'abc-12-xyz', 'abc-123-xy', 'abc123xyz'].map(async (code) => {
        const response = await fetch(`${server.base}/api/rooms/${code}/token`);
        assert.equal(response.status, 400, `code ${code} must be rejected`);
      }),
    );
  } finally {
    await server.close();
  }
});

test('spectator token route mints a usable token for an active room', async () => {
  const server = await startTestServer();
  try {
    const room = await createRoom(server.base);
    const response = await fetch(`${server.base}/api/rooms/${room.code}/token`);
    assert.equal(response.status, 200);
    const body: unknown = await response.json();
    if (!isJsonObject(body)) {
      throw new Error('token response is not a JSON object');
    }
    const token = readString(body, 'token');
    const identity = readString(body, 'identity');
    const livekitUrl = readString(body, 'livekitUrl');
    assert.ok(token !== undefined);
    assert.ok(identity !== undefined);
    assert.ok(livekitUrl !== undefined);
    assert.equal(token.split('.').length, 3);
    assert.match(identity, new RegExp(`^spectator-${room.code}-\\d+$`, 'u'));
    assert.equal(livekitUrl, 'ws://localhost:7880');
  } finally {
    await server.close();
  }
});

test('spectator token route rejects an unknown room', async () => {
  const server = await startTestServer();
  try {
    const response = await fetch(`${server.base}/api/rooms/zzz-999-qqq/token`);
    assert.equal(response.status, 404);
  } finally {
    await server.close();
  }
});

test('DELETE /api/rooms requires the private close key and invalidates the link', async () => {
  const server = await startTestServer();
  try {
    const room = await createRoom(server.base);
    const forbidden = await fetch(`${server.base}/api/rooms/${room.code}`, {
      headers: { 'X-Room-Close-Key': 'wrong-key' },
      method: 'DELETE',
    });
    assert.equal(forbidden.status, 403);
    assert.ok(server.client.rooms.has(room.code));

    const closed = await fetch(`${server.base}/api/rooms/${room.code}`, {
      headers: { 'X-Room-Close-Key': room.closeKey },
      method: 'DELETE',
    });
    assert.equal(closed.status, 204);
    assert.ok(!server.client.rooms.has(room.code));

    const join = await fetch(`${server.base}/api/rooms/${room.code}/token`);
    assert.equal(join.status, 404);
  } finally {
    await server.close();
  }
});

test('failed LiveKit deletion keeps the room active for retry', async () => {
  const server = await startTestServer();
  try {
    const room = await createRoom(server.base);
    server.client.shouldFailDeletion = true;
    const response = await fetch(`${server.base}/api/rooms/${room.code}`, {
      headers: { 'X-Room-Close-Key': room.closeKey },
      method: 'DELETE',
    });
    assert.equal(response.status, 503);
    assert.ok(server.client.rooms.has(room.code));

    const join = await fetch(`${server.base}/api/rooms/${room.code}/token`);
    assert.equal(join.status, 200);
  } finally {
    await server.close();
  }
});

test('startup deletes legacy tagged rooms without a persisted close key and leaves unrelated rooms', async () => {
  const client = new FakeRoomClient();
  client.rooms.set('abc-123-xyz', { metadata: JSON.stringify({ owner: 'slopcast' }), name: 'abc-123-xyz' });
  client.rooms.set('other-room', { metadata: JSON.stringify({ owner: 'another-app' }), name: 'other-room' });
  const server = await startTestServer(client);
  try {
    const health = await fetch(`${server.base}/api/health`);
    assert.equal(health.status, 200);
    assert.ok(!client.rooms.has('abc-123-xyz'));
    assert.ok(client.rooms.has('other-room'));
  } finally {
    await server.close();
  }
});

const refreshPresenter = (base: string, code: string, closeKey: string): Promise<Response> =>
  fetch(`${base}/api/rooms/${code}/presenter-token`, {
    headers: { 'X-Room-Close-Key': closeKey },
    method: 'POST',
  });

test('room metadata persists only a hash of the close key', async () => {
  const server = await startTestServer();
  try {
    const room = await createRoom(server.base);
    const metadata = server.client.rooms.get(room.code)?.metadata ?? '';
    assert.ok(!metadata.includes(room.closeKey));
    const parsed = parseRoomMetadata(metadata);
    assert.equal(parsed.kind, 'slopcast');
    assert.equal(parsed.room.closeKeyHash, hashCloseKey(room.closeKey));
    assert.match(parsed.room.presenterIdentity, new RegExp(`^presenter-${room.code}-\\d+$`, 'u'));
  } finally {
    await server.close();
  }
});

test('a restarted server adopts live rooms instead of deleting them', async () => {
  const client = new FakeRoomClient();
  const first = await startTestServer(client);
  let room: { closeKey: string; code: string } | undefined = undefined;
  try {
    room = await createRoom(first.base);
  } finally {
    await first.close();
  }

  const second = await startTestServer(client);
  try {
    const join = await fetch(`${second.base}/api/rooms/${room.code}/token`);
    assert.equal(join.status, 200);
    assert.ok(client.rooms.has(room.code));

    const heartbeat = await refreshPresenter(second.base, room.code, room.closeKey);
    assert.equal(heartbeat.status, 200);

    const closed = await fetch(`${second.base}/api/rooms/${room.code}`, {
      headers: { 'X-Room-Close-Key': room.closeKey },
      method: 'DELETE',
    });
    assert.equal(closed.status, 204);
    assert.ok(!client.rooms.has(room.code));
  } finally {
    await second.close();
  }
});

test('presenter heartbeat requires the close key and reissues the original identity', async () => {
  const server = await startTestServer();
  try {
    const room = await createRoom(server.base);
    const forbidden = await refreshPresenter(server.base, room.code, 'wrong-key');
    assert.equal(forbidden.status, 403);

    const response = await refreshPresenter(server.base, room.code, room.closeKey);
    assert.equal(response.status, 200);
    const body: unknown = await response.json();
    if (!isJsonObject(body)) {
      throw new Error('heartbeat response is not a JSON object');
    }
    const parsed = parseRoomMetadata(server.client.rooms.get(room.code)?.metadata ?? '');
    assert.equal(parsed.kind, 'slopcast');
    assert.equal(readString(body, 'identity'), parsed.room.presenterIdentity);
    assert.equal(readString(body, 'token')?.split('.').length, 3);
    assert.equal(readString(body, 'livekitUrl'), 'ws://localhost:7880');
  } finally {
    await server.close();
  }
});

test('presenter heartbeat reports a closed room as 404', async () => {
  const server = await startTestServer();
  try {
    const room = await createRoom(server.base);
    await fetch(`${server.base}/api/rooms/${room.code}`, {
      headers: { 'X-Room-Close-Key': room.closeKey },
      method: 'DELETE',
    });
    const response = await refreshPresenter(server.base, room.code, room.closeKey);
    assert.equal(response.status, 404);
  } finally {
    await server.close();
  }
});

test('room routes answer 503 rather than 404 while previous rooms cannot be recovered', async () => {
  const client = new FakeRoomClient();
  client.listRooms = () => Promise.reject(new Error('LiveKit unreachable'));
  const server = await startTestServer(client);
  try {
    const join = await fetch(`${server.base}/api/rooms/abc-123-xyz/token`);
    assert.equal(join.status, 503);
    const heartbeat = await refreshPresenter(server.base, 'abc-123-xyz', 'key');
    assert.equal(heartbeat.status, 503);
  } finally {
    await server.close();
  }
});

const sweptRoom = (lastHeartbeatAt: number | null, presenterMissingSince: number | null): ActiveRoom => ({
  closeKeyHash: hashCloseKey('key'),
  lastHeartbeatAt,
  presenterIdentity: 'presenter-abc-123-xyz-1',
  presenterMissingSince,
});

test('sweep keeps a room whose presenter is heartbeating while away from LiveKit', async () => {
  const client = new FakeRoomClient();
  client.rooms.set('abc-123-xyz', { metadata: '', name: 'abc-123-xyz' });
  const now = 1_000_000;
  const room = sweptRoom(now - 1000, now - PRESENTER_GRACE_MS * 2);
  const activeRooms = new Map([['abc-123-xyz', room]]);
  await sweepActiveRoom(client, activeRooms, 'abc-123-xyz', room, now);
  assert.ok(activeRooms.has('abc-123-xyz'));
  assert.ok(client.rooms.has('abc-123-xyz'));
  assert.equal(room.presenterMissingSince, null);
});

test('sweep closes a room only after the presenter is gone for the full grace period', async () => {
  const client = new FakeRoomClient();
  client.rooms.set('abc-123-xyz', { metadata: '', name: 'abc-123-xyz' });
  client.participants.set('abc-123-xyz', [{ identity: 'spectator-abc-123-xyz-1' }]);
  const start = 1_000_000;
  const room = sweptRoom(start - PRESENTER_GRACE_MS, null);
  const activeRooms = new Map([['abc-123-xyz', room]]);

  await sweepActiveRoom(client, activeRooms, 'abc-123-xyz', room, start);
  await sweepActiveRoom(client, activeRooms, 'abc-123-xyz', room, start + PRESENTER_GRACE_MS - 1);
  assert.ok(activeRooms.has('abc-123-xyz'));

  await sweepActiveRoom(client, activeRooms, 'abc-123-xyz', room, start + PRESENTER_GRACE_MS);
  assert.ok(!activeRooms.has('abc-123-xyz'));
  assert.ok(!client.rooms.has('abc-123-xyz'));
});
