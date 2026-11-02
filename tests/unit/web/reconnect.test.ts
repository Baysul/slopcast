import assert from 'node:assert/strict';

import { test } from 'vite-plus/test';

import {
  DisconnectReason,
  RECONNECT_BASE_MS,
  RECONNECT_MAX_MS,
  TokenRequestError,
  classifyDisconnect,
  isRoomGone,
  reconnectDelayMs,
} from '../../../apps/web/src/lib/reconnect.ts';

test('spectators rejoin after transport failures and stop only when the room is gone', () => {
  assert.equal(classifyDisconnect(DisconnectReason.SIGNAL_CLOSE), 'rejoin');
  assert.equal(classifyDisconnect(DisconnectReason.UNKNOWN_REASON), 'rejoin');
  assert.equal(classifyDisconnect(DisconnectReason.SERVER_SHUTDOWN), 'rejoin');
  assert.equal(classifyDisconnect(DisconnectReason.JOIN_FAILURE), 'rejoin');
  assert.equal(classifyDisconnect(undefined), 'rejoin');
  assert.equal(classifyDisconnect(DisconnectReason.ROOM_DELETED), 'ended');
  assert.equal(classifyDisconnect(DisconnectReason.ROOM_CLOSED), 'ended');
  assert.equal(classifyDisconnect(DisconnectReason.PARTICIPANT_REMOVED), 'ended');
  assert.equal(classifyDisconnect(DisconnectReason.CLIENT_INITIATED), 'ignore');
});

test('rejoin delay grows exponentially with jitter and stays under the cap', () => {
  assert.equal(
    reconnectDelayMs(0, () => 0),
    RECONNECT_BASE_MS / 2,
  );
  assert.equal(
    reconnectDelayMs(0, () => 1),
    RECONNECT_BASE_MS,
  );
  assert.equal(
    reconnectDelayMs(3, () => 1),
    RECONNECT_BASE_MS * 8,
  );
  assert.equal(
    reconnectDelayMs(20, () => 1),
    RECONNECT_MAX_MS,
  );
  assert.equal(
    reconnectDelayMs(20, () => 0),
    RECONNECT_MAX_MS / 2,
  );
  for (let attempt = 0; attempt < 30; attempt++) {
    const delay = reconnectDelayMs(attempt);
    assert.ok(delay > 0 && delay <= RECONNECT_MAX_MS);
  }
});

test('only a closed or malformed room stops spectator retries', () => {
  assert.ok(isRoomGone(new TokenRequestError('Room not found or closed', 404)));
  assert.ok(isRoomGone(new TokenRequestError('Invalid room code format', 400)));
  assert.ok(!isRoomGone(new TokenRequestError('Streaming service is temporarily unavailable', 503)));
  assert.ok(!isRoomGone(new TokenRequestError('Too many token requests', 429)));
  assert.ok(!isRoomGone(new TypeError('Failed to fetch')));
});
