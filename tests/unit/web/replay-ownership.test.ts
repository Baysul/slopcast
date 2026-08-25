import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  REPLAY_BLOCKED_MESSAGE,
  type ReplayLockRequest,
  ReplayOwnership,
  type ReplayOwnershipState,
} from '../../../apps/web/src/components/replay/replay-ownership.ts';

interface PendingLockRequest {
  callback: () => Promise<void>;
  signal: AbortSignal;
  resolve: () => void;
  reject: (error: Error) => void;
  isStarted: boolean;
}

class LockQueue {
  private readonly pending: PendingLockRequest[] = [];
  private readonly states = new Set<'held'>();

  readonly request: ReplayLockRequest = (_name, options, callback) =>
    new Promise<void>((resolve, reject) => {
      const pendingRequest: PendingLockRequest = {
        callback,
        signal: options.signal,
        resolve,
        reject,
        isStarted: false,
      };
      const handleAbort = (): void => {
        if (pendingRequest.isStarted) return;

        const requestIndex = this.pending.indexOf(pendingRequest);
        if (requestIndex >= 0) {
          this.pending.splice(requestIndex, 1);
        }
        reject(new DOMException('Replay ownership request aborted', 'AbortError'));
      };
      options.signal.addEventListener('abort', handleAbort, { once: true });
      this.pending.push(pendingRequest);
      this.grantNext();
    });

  private grantNext(): void {
    if (this.states.has('held')) return;

    const pendingRequest = this.pending.shift();
    if (!pendingRequest) return;
    if (pendingRequest.signal.aborted) {
      this.grantNext();
      return;
    }

    pendingRequest.isStarted = true;
    this.states.add('held');
    pendingRequest.callback().then(
      () => {
        this.states.delete('held');
        pendingRequest.resolve();
        this.grantNext();
      },
      (error: Error) => {
        this.states.delete('held');
        pendingRequest.reject(error);
        this.grantNext();
      },
    );
  }
}

const flushTasks = (): Promise<void> => new Promise((resolve) => setImmediate(resolve));

const createOwnership = (lockQueue: LockQueue, states: ReplayOwnershipState[]): ReplayOwnership =>
  new ReplayOwnership(lockQueue.request, (state) => states.push(state));

test('only the oldest eligible tab owns replay', async () => {
  const lockQueue = new LockQueue();
  const firstStates: ReplayOwnershipState[] = [];
  const secondStates: ReplayOwnershipState[] = [];
  const thirdStates: ReplayOwnershipState[] = [];
  const first = createOwnership(lockQueue, firstStates);
  const second = createOwnership(lockQueue, secondStates);
  const third = createOwnership(lockQueue, thirdStates);

  first.claim();
  second.claim();
  third.claim();

  assert.equal(first.getState().status, 'owned');
  assert.equal(second.getState().status, 'waiting');
  assert.equal(third.getState().status, 'waiting');

  first.release();
  await flushTasks();

  assert.equal(first.getState().status, 'idle');
  assert.equal(second.getState().status, 'owned');
  assert.equal(third.getState().status, 'waiting');

  second.release();
  await flushTasks();

  assert.equal(second.getState().status, 'idle');
  assert.equal(third.getState().status, 'owned');
  assert.deepEqual(
    secondStates.map((state) => state.status),
    ['waiting', 'owned', 'idle'],
  );

  third.destroy();
});

test('a tab that becomes ineligible leaves the handoff queue', async () => {
  const lockQueue = new LockQueue();
  const first = createOwnership(lockQueue, []);
  const secondStates: ReplayOwnershipState[] = [];
  const second = createOwnership(lockQueue, secondStates);

  first.claim();
  second.claim();
  second.release();
  await flushTasks();
  first.release();
  await flushTasks();

  assert.equal(second.getState().status, 'idle');
  assert.equal(
    secondStates.some((state) => state.status === 'owned'),
    false,
  );
});

test('destroying the owner hands replay to the oldest waiter', async () => {
  const lockQueue = new LockQueue();
  const first = createOwnership(lockQueue, []);
  const second = createOwnership(lockQueue, []);

  first.claim();
  second.claim();
  first.destroy();
  await flushTasks();

  assert.equal(second.getState().status, 'owned');
  second.destroy();
});

test('lock failures stop retrying and explain why replay failed', async () => {
  const states: ReplayOwnershipState[] = [];
  const failedRequest: ReplayLockRequest = () => Promise.reject(new Error('Lock service failed'));
  const ownership = new ReplayOwnership(failedRequest, (state) => states.push(state));

  ownership.claim();
  await flushTasks();

  assert.deepEqual(
    states.map((state) => state.status),
    ['waiting', 'failed'],
  );
  assert.deepEqual(ownership.getState(), { status: 'failed', reason: 'Lock service failed' });
});

test('missing Web Locks fails closed', () => {
  const ownership = new ReplayOwnership(null, () => undefined);

  ownership.claim();

  assert.deepEqual(ownership.getState(), {
    status: 'unsupported',
    reason: 'Cross-tab replay ownership requires Web Locks in a secure context',
  });
});

test('blocked replay state provides the settings notice', () => {
  assert.equal(REPLAY_BLOCKED_MESSAGE, 'Replay is active in another tab.');
});
