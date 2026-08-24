import assert from 'node:assert/strict';
import type { Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import { test } from 'node:test';
import express from 'express';

import { initRoutes, type RoomClient, toHttpUrl, toWsUrl } from '../../../apps/server/src/routes.js';

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
  readonly participants = new Map<string, Array<{ identity: string }>>();
  shouldFailDeletion: boolean = false;

  async createRoom(options: { name: string; metadata?: string }): Promise<{ name: string; metadata: string }> {
    const room = { name: options.name, metadata: options.metadata ?? '' };
    this.rooms.set(room.name, room);
    return room;
  }

  async deleteRoom(roomName: string): Promise<void> {
    if (this.shouldFailDeletion) throw new Error('deletion unavailable');
    if (!this.rooms.delete(roomName)) throw new Error('room not found');
    this.participants.delete(roomName);
  }

  async listParticipants(roomName: string): Promise<Array<{ identity: string }>> {
    if (!this.rooms.has(roomName)) throw new Error('room not found');
    return this.participants.get(roomName) ?? [];
  }

  async listRooms(names?: string[]): Promise<Array<{ name: string; metadata: string }>> {
    const rooms = [...this.rooms.values()];
    if (!names?.length) return rooms;
    const requestedNames = new Set(names);
    return rooms.filter((room) => requestedNames.has(room.name));
  }
}

interface TestServer {
  base: string;
  client: FakeRoomClient;
  close: () => Promise<void>;
}

async function startTestServer(client = new FakeRoomClient()): Promise<TestServer> {
  const app = express();
  app.use(express.json());
  app.use(initRoutes('ws://localhost:7880', 'devkey', 'secret', 'http://localhost:3000', undefined, client));
  const server: Server = await new Promise((resolve) => {
    const listeningServer = app.listen(0, '127.0.0.1', () => resolve(listeningServer));
  });
  // SAFETY: this TCP server listens on an ephemeral IP port, so address() returns AddressInfo.
  const address = server.address() as AddressInfo;

  return {
    base: `http://127.0.0.1:${address.port}`,
    client,
    close: () =>
      new Promise((resolve) => {
        server.closeAllConnections();
        server.close(() => resolve());
      }),
  };
}

async function createRoom(base: string): Promise<{ closeKey: string; code: string }> {
  const response = await fetch(`${base}/api/rooms`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', 'X-Client-Origin': 'desktop' },
  });
  assert.equal(response.status, 200);
  // SAFETY: this successful route response has the colocated room JSON contract.
  const body = (await response.json()) as { closeKey: string; code: string };
  assert.match(body.code, /^[a-z]{3}-\d{3}-[a-z]{3}$/);
  assert.ok(body.closeKey.length > 20);
  return body;
}

test('POST /api/rooms without the desktop header is forbidden', async () => {
  const server = await startTestServer();
  try {
    const response = await fetch(`${server.base}/api/rooms`, { method: 'POST' });
    assert.equal(response.status, 403);
    // SAFETY: this route's 403 response has the colocated JSON error contract.
    const body = (await response.json()) as { error: string };
    assert.match(body.error, /desktop clients/i);
  } finally {
    await server.close();
  }
});

test('spectator token route rejects malformed room codes with 400', async () => {
  const server = await startTestServer();
  try {
    for (const code of ['ABC-123-XYZ', 'ab-123-xyz', 'abc-12-xyz', 'abc-123-xy', 'abc123xyz']) {
      const response = await fetch(`${server.base}/api/rooms/${code}/token`);
      assert.equal(response.status, 400, `code ${code} must be rejected`);
    }
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
    // SAFETY: this successful route response has the colocated token JSON contract.
    const body = (await response.json()) as { identity: string; livekitUrl: string; token: string };
    assert.equal(body.token.split('.').length, 3);
    assert.match(body.identity, new RegExp(`^spectator-${room.code}-\\d+$`));
    assert.equal(body.livekitUrl, 'ws://localhost:7880');
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
      method: 'DELETE',
      headers: { 'X-Room-Close-Key': 'wrong-key' },
    });
    assert.equal(forbidden.status, 403);
    assert.ok(server.client.rooms.has(room.code));

    const closed = await fetch(`${server.base}/api/rooms/${room.code}`, {
      method: 'DELETE',
      headers: { 'X-Room-Close-Key': room.closeKey },
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
      method: 'DELETE',
      headers: { 'X-Room-Close-Key': room.closeKey },
    });
    assert.equal(response.status, 503);
    assert.ok(server.client.rooms.has(room.code));

    const join = await fetch(`${server.base}/api/rooms/${room.code}/token`);
    assert.equal(join.status, 200);
  } finally {
    await server.close();
  }
});

test('startup deletes tagged Slopcast rooms without touching unrelated rooms', async () => {
  const client = new FakeRoomClient();
  client.rooms.set('abc-123-xyz', { name: 'abc-123-xyz', metadata: JSON.stringify({ owner: 'slopcast' }) });
  client.rooms.set('other-room', { name: 'other-room', metadata: JSON.stringify({ owner: 'another-app' }) });
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
