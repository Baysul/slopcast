import assert from 'node:assert/strict';
import { test } from 'node:test';

import { checkApiEndpoint, normalizeApiEndpoint } from '../../../apps/desktop/src/renderer/utils/apiEndpoint.ts';

test('normalizeApiEndpoint accepts HTTP base URLs and normalizes trailing slashes', () => {
  assert.deepEqual(normalizeApiEndpoint(' https://example.com/slopcast/// '), {
    ok: true,
    endpoint: 'https://example.com/slopcast',
  });
  assert.deepEqual(normalizeApiEndpoint('http://localhost:3001/'), {
    ok: true,
    endpoint: 'http://localhost:3001',
  });
});

test('normalizeApiEndpoint rejects unsafe or unusable URL shapes', () => {
  for (const endpoint of [
    '',
    'not-a-url',
    'ftp://example.com',
    'https://user:secret@example.com',
    'https://example.com?mode=test',
    'https://example.com#health',
  ]) {
    const result = normalizeApiEndpoint(endpoint);
    assert.equal(result.ok, false, `${endpoint || 'empty input'} must be rejected`);
    if (!result.ok) assert.equal(result.kind, 'invalid');
  }
});

test('checkApiEndpoint requires the Slopcast healthy response contract', async () => {
  const controller = new AbortController();
  const healthyFetch: typeof fetch = async () =>
    new Response(JSON.stringify({ status: 'ok' }), { status: 200, headers: { 'Content-Type': 'application/json' } });
  const unhealthyFetch: typeof fetch = async () =>
    new Response(JSON.stringify({ status: 'degraded' }), {
      status: 503,
      headers: { 'Content-Type': 'application/json' },
    });

  assert.deepEqual(await checkApiEndpoint('https://example.com', controller.signal, healthyFetch), {
    ok: true,
    endpoint: 'https://example.com',
  });
  assert.deepEqual(await checkApiEndpoint('https://example.com', controller.signal, unhealthyFetch), {
    ok: false,
    kind: 'unhealthy',
    message: 'The API is online, but its LiveKit service is unavailable.',
  });
});

test('checkApiEndpoint classifies unexpected and unreachable responses', async () => {
  const controller = new AbortController();
  const wrongBodyFetch: typeof fetch = async () =>
    new Response(JSON.stringify({ status: 'different-service' }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    });
  const unreachableFetch: typeof fetch = async () => {
    throw new TypeError('network failed');
  };

  const wrongBody = await checkApiEndpoint('https://example.com', controller.signal, wrongBodyFetch);
  assert.equal(wrongBody.ok, false);
  if (!wrongBody.ok) assert.equal(wrongBody.kind, 'unexpected');

  const unreachable = await checkApiEndpoint('https://example.com', controller.signal, unreachableFetch);
  assert.equal(unreachable.ok, false);
  if (!unreachable.ok) assert.equal(unreachable.kind, 'unreachable');
});
