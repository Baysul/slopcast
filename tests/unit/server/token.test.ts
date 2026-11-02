import assert from 'node:assert/strict';

import { isJsonObject, readBoolean, readFiniteNumber, readString } from '@slopcast/shared-types';
import type { JsonObject } from '@slopcast/shared-types';
import { AccessToken, TokenVerifier } from 'livekit-server-sdk';
import { test } from 'vite-plus/test';

import { presenterToken, spectatorToken } from '../../../apps/server/src/token.js';

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

const readVideoGrant = (value: JsonObject): VideoGrant => ({
  canPublish: readBoolean(value, 'canPublish') ?? false,
  canPublishData: readBoolean(value, 'canPublishData') ?? false,
  canSubscribe: readBoolean(value, 'canSubscribe') ?? false,
  room: readString(value, 'room') ?? '',
  roomJoin: readBoolean(value, 'roomJoin') ?? false,
});

const verifyToken = async (token: string): Promise<ClaimGrants> => {
  const verifier = new TokenVerifier(API_KEY, API_SECRET);
  const claims: unknown = await verifier.verify(token);
  if (!isJsonObject(claims)) {
    throw new Error('Token claims are not a JSON object');
  }
  const video: unknown = claims.video;
  if (!isJsonObject(video)) {
    throw new Error('Token claims have no video grant');
  }
  const grants: ClaimGrants = {
    sub: readString(claims, 'sub') ?? '',
    video: readVideoGrant(video),
  };
  const exp = readFiniteNumber(claims, 'exp');
  const nbf = readFiniteNumber(claims, 'nbf');
  if (exp !== undefined) {
    grants.exp = exp;
  }
  if (nbf !== undefined) {
    grants.nbf = nbf;
  }
  return grants;
};

test('presenter token is publisher-only', async () => {
  const token = await presenterToken(API_KEY, API_SECRET, 'abc-123-xyz', 'presenter-abc-123-xyz-1');
  const grants = await verifyToken(token);
  assert.equal(grants.video.room, 'abc-123-xyz');
  assert.equal(grants.video.roomJoin, true);
  assert.equal(grants.video.canPublish, true);
  assert.equal(grants.video.canSubscribe, false);
  assert.equal(grants.video.canPublishData, false);
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
  const parts = token.split('.');
  const header = parts.at(0);
  const payload = parts.at(1);
  const signature = parts.at(2);
  if (header === undefined || payload === undefined || signature === undefined) {
    throw new Error('Unexpected JWT format');
  }

  const tamperedPayload = Buffer.from(
    JSON.stringify({ ...JSON.parse(Buffer.from(payload, 'base64url').toString()), video: { canPublish: true } }),
  ).toString('base64url');
  await assert.rejects(verifier.verify(`${header}.${tamperedPayload}.${signature}`), /signature|invalid/iu);
});

test('an unverifiable key pair is rejected', async () => {
  const token = await presenterToken('devkey', 'secret', 'abc-123-xyz', 'presenter-wrong-key');
  const wrongVerifier = new TokenVerifier('other-key', 'other-secret');
  await assert.rejects(wrongVerifier.verify(token));
});

test('AccessToken can still be constructed like production code', async () => {
  const at = new AccessToken(API_KEY, API_SECRET, { identity: 'probe', ttl: '6h' });
  at.addGrant({ canPublish: true, room: 'abc-123-xyz', roomJoin: true });
  assert.equal(at.toJwt instanceof Function, true);
  assert.equal((await at.toJwt()).split('.').length, 3);
});
