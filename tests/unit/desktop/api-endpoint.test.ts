import assert from 'node:assert/strict';

import { test } from 'vite-plus/test';

import { checkApiEndpoint, normalizeApiEndpoint } from '../../../apps/desktop/src/renderer/utils/apiEndpoint.ts';

test('normalizeApiEndpoint accepts HTTP base URLs and normalizes trailing slashes', () => {
  assert.deepEqual(normalizeApiEndpoint(' https://example.com/slopcast/// '), {
    endpoint: 'https://example.com/slopcast',
    ok: true,
  });
  assert.deepEqual(normalizeApiEndpoint('http://localhost:3001/'), {
    endpoint: 'http://localhost:3001',
    ok: true,
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
    assert.equal(result.kind, 'invalid');
  }
});

test('checkApiEndpoint requires the Slopcast healthy response contract', async () => {
  const controller = new AbortController();
  const healthyFetch: typeof fetch = () =>
    Promise.resolve(
      new Response(JSON.stringify({ status: 'ok' }), { headers: { 'Content-Type': 'application/json' }, status: 200 }),
    );
  const unhealthyFetch: typeof fetch = () =>
    Promise.resolve(
      new Response(JSON.stringify({ status: 'degraded' }), {
        headers: { 'Content-Type': 'application/json' },
        status: 503,
      }),
    );

  assert.deepEqual(await checkApiEndpoint('https://example.com', controller.signal, healthyFetch), {
    endpoint: 'https://example.com',
    ok: true,
  });
  assert.deepEqual(await checkApiEndpoint('https://example.com', controller.signal, unhealthyFetch), {
    kind: 'unhealthy',
    message: 'The API is online, but its LiveKit service is unavailable.',
    ok: false,
  });
});

test('checkApiEndpoint classifies unexpected and unreachable responses', async () => {
  const controller = new AbortController();
  const wrongBodyFetch: typeof fetch = () =>
    Promise.resolve(
      new Response(JSON.stringify({ status: 'different-service' }), {
        headers: { 'Content-Type': 'application/json' },
        status: 200,
      }),
    );
  const unreachableFetch: typeof fetch = () => Promise.reject(new TypeError('network failed'));

  const wrongBody = await checkApiEndpoint('https://example.com', controller.signal, wrongBodyFetch);
  assert.equal(wrongBody.ok, false);
  assert.equal(wrongBody.kind, 'unexpected');

  const unreachable = await checkApiEndpoint('https://example.com', controller.signal, unreachableFetch);
  assert.equal(unreachable.ok, false);
  assert.equal(unreachable.kind, 'unreachable');
});
