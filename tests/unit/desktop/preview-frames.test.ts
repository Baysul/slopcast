import assert from 'node:assert/strict';

import { afterEach, test, vi } from 'vite-plus/test';

import type { PreviewFrame } from '../../../apps/desktop/src/renderer/types/index.ts';
import { startPreviewPolling } from '../../../apps/desktop/src/renderer/utils/preview-frames.ts';

let stop: (() => void) | undefined;
afterEach(() => {
  stop?.();
  stop = undefined;
  vi.unstubAllGlobals();
});

const mockAnimationFrames = () => {
  const pending = new Map<number, FrameRequestCallback>();
  let nextId = 0;
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
    const id = ++nextId;
    pending.set(id, callback);
    return id;
  });
  vi.stubGlobal('cancelAnimationFrame', (id: number) => pending.delete(id));
  return {
    pending,
    run: () => {
      const first = pending.entries().next().value;
      assert.ok(first, 'a preview poll must be scheduled');
      const [id, callback] = first;
      pending.delete(id);
      callback(0);
    },
  };
};

const payload = (ptsUs: number): ArrayBuffer => {
  const bytes = new ArrayBuffer(20);
  const view = new DataView(bytes);
  view.setBigUint64(0, BigInt(ptsUs), true);
  view.setUint32(8, 1, true);
  view.setUint32(12, 1, true);
  new Uint8Array(bytes, 16).set([1, 2, 3, 255]);
  return bytes;
};

test('preview polling requests changes after the last frame and skips empty responses', async () => {
  const frames = mockAnimationFrames();
  const fetchMock = vi
    .fn<typeof fetch>()
    .mockResolvedValueOnce(new Response(payload(0)))
    .mockResolvedValueOnce(new Response(null, { status: 204 }))
    .mockResolvedValueOnce(new Response(payload(1)));
  vi.stubGlobal('fetch', fetchMock);
  const rendered: PreviewFrame[] = [];
  stop = startPreviewPolling((frame) => {
    rendered.push(frame);
  });

  frames.run();
  await vi.waitFor(() => {
    assert.equal(frames.pending.size, 1);
  });
  assert.equal(rendered[0]?.ptsUs, 0, 'a zero timestamp is a valid first frame');
  frames.run();
  await vi.waitFor(() => {
    assert.equal(frames.pending.size, 1);
  });
  assert.equal(rendered.length, 1);
  assert.equal(fetchMock.mock.calls[1]?.[0], 'http://frame.localhost/frame.bin?after=0');
  frames.run();
  await vi.waitFor(() => {
    assert.equal(rendered.length, 2);
  });
  assert.equal(rendered[1]?.ptsUs, 1);
  assert.deepEqual(Array.from(rendered[1]?.data ?? []), [1, 2, 3, 255]);
});

test('stopping preview cancels the scheduled poll before it makes a request', () => {
  const frames = mockAnimationFrames();
  const fetchMock = vi.fn<typeof fetch>();
  vi.stubGlobal('fetch', fetchMock);
  stop = startPreviewPolling(() => assert.fail('stopped preview rendered a frame'));
  stop();
  assert.equal(frames.pending.size, 0);
  assert.equal(fetchMock.mock.calls.length, 0);
});

test('stopping preview aborts an in-flight request and rejects late response data', async () => {
  const frames = mockAnimationFrames();
  let resolveBody: ((bytes: ArrayBuffer) => void) | undefined;
  const body = new Promise<ArrayBuffer>((resolve) => {
    resolveBody = resolve;
  });
  const response = new Response();
  const readBody = vi.spyOn(response, 'arrayBuffer').mockReturnValue(body);
  const fetchMock = vi.fn<typeof fetch>().mockResolvedValue(response);
  vi.stubGlobal('fetch', fetchMock);
  stop = startPreviewPolling(() => assert.fail('stopped preview rendered a late frame'));
  frames.run();
  await vi.waitFor(() => {
    assert.equal(readBody.mock.calls.length, 1);
  });
  assert.equal(frames.pending.size, 0, 'requests must remain serialized while reading a frame');

  stop();
  assert.equal(fetchMock.mock.calls[0]?.[1]?.signal?.aborted, true);
  resolveBody?.(payload(1));
  await body;
  await Promise.resolve();
  assert.equal(frames.pending.size, 0);
});
