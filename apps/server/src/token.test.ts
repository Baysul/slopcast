import assert from 'node:assert/strict';
import { test } from 'node:test';
import { AccessToken, TokenVerifier } from 'livekit-server-sdk';

import { presenterToken, spectatorToken } from './token.js';

const API_KEY = 'devkey';
const API_SECRET = 'secret';

interface VideoGrant {
  room: string;
  roomJoin: boolean;
  canPublish: boolean;
  canSubscribe: boolean;
  canPublishData: boolean;
}

interface ClaimGrants {
  sub: string;
  video: VideoGrant;
  exp?: number;
  nbf?: number;
}

async function verifyToken(token: string): Promise<ClaimGrants> {
  const verifier = new TokenVerifier(API_KEY, API_SECRET);
  return (await verifier.verify(token)) as ClaimGrants;
}

test('presenter token carries publish grants for the room', async () => {
  const token = await presenterToken(API_KEY, API_SECRET, 'abc-123-xyz', 'presenter-abc-123-xyz-1');
  const grants = await verifyToken(token);
  assert.equal(grants.video.room, 'abc-123-xyz');
  assert.equal(grants.video.roomJoin, true);
  assert.equal(grants.video.canPublish, true);
  assert.equal(grants.video.canSubscribe, true);
  assert.equal(grants.video.canPublishData, true);
});

test('spectator token cannot publish or publish data', async () => {
  const token = await spectatorToken(API_KEY, API_SECRET, 'abc-123-xyz', 'spectator-abc-123-xyz-2');
  const grants = await verifyToken(token);
  assert.equal(grants.video.room, 'abc-123-xyz');
  assert.equal(grants.video.roomJoin, true);
  assert.equal(grants.video.canPublish, false);
  assert.equal(grants.video.canSubscribe, true);
  assert.equal(grants.video.canPublishData, false);
});

test('tokens expire after the 6h ttl', async () => {
  const token = await presenterToken(API_KEY, API_SECRET, 'abc-123-xyz', 'presenter-ttl');
  const grants = await verifyToken(token);
  assert.ok(grants.nbf !== undefined);
  assert.ok(grants.exp !== undefined);
  assert.equal(grants.exp - grants.nbf, 6 * 60 * 60);
});

test('token identities are embedded verbatim', async () => {
  const identity = 'presenter-abc-123-xyz-1700000000000';
  const token = await presenterToken(API_KEY, API_SECRET, 'abc-123-xyz', identity);
  const grants = await verifyToken(token);
  assert.equal(grants.sub, identity);
});

test('tokens are signed so a tampered grant fails verification', async () => {
  const token = await spectatorToken(API_KEY, API_SECRET, 'abc-123-xyz', 'spectator-tamper');
  const verifier = new TokenVerifier(API_KEY, API_SECRET);
  // Flipping the publish grant in the JWT payload must invalidate the signature.
  const [header, payload, signature] = token.split('.');
  const tamperedPayload = Buffer.from(
    JSON.stringify({ ...JSON.parse(Buffer.from(payload, 'base64url').toString()), video: { canPublish: true } }),
  ).toString('base64url');
  await assert.rejects(verifier.verify(`${header}.${tamperedPayload}.${signature}`), /signature|invalid/i);
});

test('an unverifiable key pair is rejected', async () => {
  const token = await presenterToken('devkey', 'secret', 'abc-123-xyz', 'presenter-wrong-key');
  const wrongVerifier = new TokenVerifier('other-key', 'other-secret');
  await assert.rejects(wrongVerifier.verify(token));
});

test('AccessToken can still be constructed like production code', async () => {
  // Guards against the SDK surface drifting (the production token.ts depends
  // on the AccessToken constructor + addGrant + toJwt contract).
  const at = new AccessToken(API_KEY, API_SECRET, { identity: 'probe', ttl: '6h' });
  at.addGrant({ roomJoin: true, room: 'abc-123-xyz', canPublish: true });
  assert.equal(typeof at.toJwt, 'function');
  assert.equal(typeof (await at.toJwt()), 'string');
});
