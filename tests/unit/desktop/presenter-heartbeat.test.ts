import assert from 'node:assert/strict';

import { test } from 'vite-plus/test';

import { heartbeatResult } from '../../../apps/desktop/src/renderer/utils/presenter-heartbeat.ts';

test('a successful heartbeat hands back the refreshed token', () => {
  assert.deepEqual(heartbeatResult(200, { identity: 'presenter-abc-123-xyz-1', token: 'jwt' }), {
    kind: 'ok',
    token: 'jwt',
  });
  assert.equal(heartbeatResult(200, {}).kind, 'retry');
});

test('the server reporting the room gone ends the presenter session', () => {
  assert.equal(heartbeatResult(404, { error: 'Room not found or closed' }).kind, 'closed');
  assert.equal(heartbeatResult(403, { error: 'Invalid room close key' }).kind, 'closed');
});

test('outages and older servers never end a live stream', () => {
  assert.equal(heartbeatResult(503, { error: 'Streaming service is temporarily unavailable' }).kind, 'retry');
  assert.equal(heartbeatResult(429, { error: 'Too many presenter heartbeats' }).kind, 'retry');
  assert.equal(heartbeatResult(502, null).kind, 'retry');
  // Express's default 404 page for a server without the heartbeat route is not JSON.
  assert.equal(heartbeatResult(404, null).kind, 'unsupported');
  assert.equal(heartbeatResult(403, null).kind, 'retry');
});
