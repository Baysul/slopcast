import assert from 'node:assert/strict';
import type { Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import { test } from 'node:test';
import express from 'express';

import { initRoutes, toHttpUrl, toWsUrl } from './routes.js';

// ── URL scheme normalization ───────────────────────────────────────────────
// The same helpers feed the LiveKit HTTP client and the WebSocket URL handed
// to presenters/spectators; a mismatch breaks room creation or joining.

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

// ── Route behavior (offline: no LiveKit server required) ──────────────────

async function startTestServer(): Promise<{ base: string; close: () => Promise<void> }> {
  const app = express();
  app.use(initRoutes('ws://localhost:7880', 'devkey', 'secret', 'http://localhost:3000'));
  const server: Server = await new Promise((resolve) => {
    const srv = app.listen(0, '127.0.0.1', () => resolve(srv));
  });
  const { port } = server.address() as AddressInfo;
  return {
    base: `http://127.0.0.1:${port}`,
    close: () =>
      new Promise((resolve) => {
        // fetch() keeps sockets alive; closeAllConnections lets the test
        // process exit instead of hanging on server.close().
        server.closeAllConnections();
        server.close(() => resolve());
      }),
  };
}

test('POST /api/rooms without the desktop header is forbidden', async () => {
  const server = await startTestServer();
  try {
    const res = await fetch(`${server.base}/api/rooms`, { method: 'POST' });
    assert.equal(res.status, 403);
    const body = (await res.json()) as { error: string };
    assert.match(body.error, /desktop clients/i);
  } finally {
    await server.close();
  }
});

test('spectator token route rejects malformed room codes with 400', async () => {
  const server = await startTestServer();
  try {
    for (const code of ['ABC-123-XYZ', 'ab-123-xyz', 'abc-12-xyz', 'abc-123-xy', 'abc123xyz']) {
      const res = await fetch(`${server.base}/api/rooms/${code}/token`);
      assert.equal(res.status, 400, `code ${code} must be rejected`);
    }
  } finally {
    await server.close();
  }
});

test('spectator token route mints a usable token for a valid code', async () => {
  const server = await startTestServer();
  try {
    const res = await fetch(`${server.base}/api/rooms/abc-123-xyz/token`);
    assert.equal(res.status, 200);
    const body = (await res.json()) as {
      token: string;
      identity: string;
      livekitUrl: string;
    };
    assert.equal(body.token.split('.').length, 3);
    assert.match(body.identity, /^spectator-abc-123-xyz-\d+$/);
    assert.equal(body.livekitUrl, 'ws://localhost:7880');
  } finally {
    await server.close();
  }
});

test('spectator token route mints a token even for an unknown room', async () => {
  // Room existence is not checked at token time — LiveKit itself rejects
  // joins to nonexistent rooms; this must stay a 200, not a 404.
  const server = await startTestServer();
  try {
    const res = await fetch(`${server.base}/api/rooms/zzz-999-qqq/token`);
    assert.equal(res.status, 200);
  } finally {
    await server.close();
  }
});
