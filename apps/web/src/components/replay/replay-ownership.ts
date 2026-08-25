const REPLAY_OWNER_LOCK = 'slopcast-viewer-replay-owner';
const UNSUPPORTED_REASON = 'Cross-tab replay ownership requires Web Locks in a secure context';

export const REPLAY_BLOCKED_MESSAGE = 'Replay is active in another tab.';

interface ExclusiveLockOptions {
  mode: 'exclusive';
  signal: AbortSignal;
}

export type ReplayLockRequest = (
  name: string,
  options: ExclusiveLockOptions,
  callback: () => Promise<void>,
) => Promise<void>;

export type ReplayOwnershipState =
  | { status: 'idle' }
  | { status: 'waiting' }
  | { status: 'owned' }
  | { status: 'unsupported'; reason: string }
  | { status: 'failed'; reason: string };

const createReleaseSignal = () => {
  let release = (): void => undefined;
  const promise = new Promise<void>((resolve) => {
    release = resolve;
  });

  return { promise, release };
};

const isAbortError = (error: Error): boolean => error.name === 'AbortError';

export class ReplayOwnership {
  private readonly requestLock: ReplayLockRequest | null;
  private readonly onChange: (state: ReplayOwnershipState) => void;
  private readonly states = new Set<'destroyed' | 'should-own'>();
  private state: ReplayOwnershipState;
  private requestController: AbortController | null = null;
  private releaseOwner: (() => void) | null = null;

  constructor(requestLock: ReplayLockRequest | null, onChange: (state: ReplayOwnershipState) => void) {
    this.requestLock = requestLock;
    this.onChange = onChange;
    this.state = requestLock ? { status: 'idle' } : { status: 'unsupported', reason: UNSUPPORTED_REASON };
  }

  getState(): ReplayOwnershipState {
    return this.state;
  }

  claim(): void {
    if (this.states.has('destroyed')) return;

    this.states.add('should-own');
    if (!this.requestLock) {
      this.setState({ status: 'unsupported', reason: UNSUPPORTED_REASON });
      return;
    }
    if (this.requestController || this.state.status === 'owned') return;

    this.beginRequest(this.requestLock);
  }

  release(): void {
    this.states.delete('should-own');
    this.requestController?.abort();
    this.releaseOwner?.();
  }

  destroy(): void {
    if (this.states.has('destroyed')) return;

    this.states.add('destroyed');
    this.release();
  }

  private beginRequest(requestLock: ReplayLockRequest): void {
    const controller = new AbortController();
    this.requestController = controller;
    this.setState({ status: 'waiting' });

    requestLock(REPLAY_OWNER_LOCK, { mode: 'exclusive', signal: controller.signal }, async () => {
      if (controller.signal.aborted || !this.states.has('should-own')) return;

      const releaseSignal = createReleaseSignal();
      this.releaseOwner = releaseSignal.release;
      this.setState({ status: 'owned' });

      await releaseSignal.promise;
      if (this.releaseOwner === releaseSignal.release) {
        this.releaseOwner = null;
      }
    }).then(
      () => this.finishRequest(controller),
      (error: Error) => this.failRequest(controller, error),
    );
  }

  private finishRequest(controller: AbortController): void {
    if (this.requestController !== controller) return;

    this.requestController = null;
    this.releaseOwner = null;
    if (this.states.has('destroyed')) return;

    this.setState({ status: 'idle' });
    if (this.states.has('should-own') && this.requestLock) {
      this.beginRequest(this.requestLock);
    }
  }

  private failRequest(controller: AbortController, error: Error): void {
    if (this.requestController !== controller) return;

    this.requestController = null;
    this.releaseOwner = null;
    if (this.states.has('destroyed')) return;
    if (isAbortError(error)) {
      this.setState({ status: 'idle' });
      if (this.states.has('should-own') && this.requestLock) {
        this.beginRequest(this.requestLock);
      }
      return;
    }

    this.states.delete('should-own');
    this.setState({ status: 'failed', reason: error.message });
  }

  private setState(state: ReplayOwnershipState): void {
    this.state = state;
    this.onChange(state);
  }
}

export const createBrowserReplayOwnership = (onChange: (state: ReplayOwnershipState) => void): ReplayOwnership => {
  if (!window.isSecureContext || !('locks' in navigator)) {
    return new ReplayOwnership(null, onChange);
  }

  const requestLock: ReplayLockRequest = (name, options, callback) =>
    navigator.locks.request(name, options, async (lock) => {
      if (!lock) return;

      await callback();
    });

  return new ReplayOwnership(requestLock, onChange);
};
