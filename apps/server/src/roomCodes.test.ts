import assert from 'node:assert/strict';
import { test } from 'node:test';
import { ROOM_CODE_RE } from '@slopcast/shared-types';

import { generateRoomCode } from './roomCodes.js';

// The server and the web join form share ROOM_CODE_RE; every generated code
// must satisfy it or join links break.

test('generated codes match the shared room-code format', () => {
  for (let i = 0; i < 200; i++) {
    assert.match(generateRoomCode(), ROOM_CODE_RE);
  }
});

test('generated codes use letters for the letter groups', () => {
  for (let i = 0; i < 100; i++) {
    const code = generateRoomCode();
    const [a, b, c] = code.split('-');
    if (a === undefined || b === undefined || c === undefined) {
      throw new Error(`Unexpected room code format: ${code}`);
    }

    assert.match(a, /^[a-z]{3}$/);
    assert.match(b, /^[0-9]{3}$/);
    assert.match(c, /^[a-z]{3}$/);
  }
});

test('generated codes are not trivially degenerate', () => {
  const seen = new Set<string>();
  for (let i = 0; i < 500; i++) {
    seen.add(generateRoomCode());
  }
  // 26^6 * 10^3 ≈ 3e11 possible codes; 500 draws colliding would be a
  // generator defect (e.g. fixed seed or zero entropy).
  assert.equal(seen.size, 500);
});
