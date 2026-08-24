interface BufferedRange {
  start: number;
  end: number;
}

interface AppendOperation {
  kind: 'append';
  blob: Blob;
  resolve: () => void;
  reject: (error: Error) => void;
}

interface RemoveOperation {
  kind: 'remove';
  start: number;
  end: number;
  resolve: () => void;
  reject: (error: Error) => void;
}

type BufferOperation = AppendOperation | RemoveOperation;

const readBufferedRange = (sourceBuffer: SourceBuffer): BufferedRange | null => {
  const { buffered } = sourceBuffer;
  if (buffered.length === 0) return null;

  return {
    start: buffered.start(0),
    end: buffered.end(buffered.length - 1),
  };
};

export class ReplayMediaBuffer {
  private readonly mediaSource = new MediaSource();
  private readonly objectUrl = URL.createObjectURL(this.mediaSource);
  private readonly operations: BufferOperation[] = [];
  private sourceBuffer: SourceBuffer | null = null;
  private activeOperation: BufferOperation | null = null;
  private isReadingBlob = false;
  private readonly states = new Set<'destroyed' | 'failed'>();
  private windowSeconds: number;

  constructor(
    private readonly video: HTMLVideoElement,
    private readonly mimeType: string,
    windowSeconds: number,
    private readonly onRangeChange: (range: BufferedRange | null) => void,
    private readonly onStoragePressure: () => number | null,
    private readonly onFailure: (error: Error) => void,
  ) {
    this.windowSeconds = windowSeconds;
    this.video.src = this.objectUrl;
    this.mediaSource.addEventListener('sourceopen', this.handleSourceOpen, { once: true });
  }

  append(blob: Blob): Promise<void> {
    if (this.states.has('destroyed')) return Promise.reject(new Error('Replay media buffer is closed'));
    if (this.states.has('failed')) return Promise.reject(new Error('Replay media buffer failed'));

    return new Promise((resolve, reject) => {
      this.operations.push({ kind: 'append', blob, resolve, reject });
      this.processNext();
    });
  }

  setWindowSeconds(windowSeconds: number): void {
    this.windowSeconds = windowSeconds;
    this.queueTrim();
  }

  getRange(): BufferedRange | null {
    if (!this.sourceBuffer) return null;
    return readBufferedRange(this.sourceBuffer);
  }

  destroy(): void {
    if (this.states.has('destroyed')) return;

    this.states.add('destroyed');
    this.mediaSource.removeEventListener('sourceopen', this.handleSourceOpen);
    if (this.sourceBuffer) {
      this.sourceBuffer.removeEventListener('updateend', this.handleUpdateEnd);
      this.sourceBuffer.removeEventListener('error', this.handleSourceBufferError);
      if (this.sourceBuffer.updating) {
        try {
          this.sourceBuffer.abort();
        } catch (error) {
          console.debug('[Replay] SourceBuffer abort during cleanup failed:', error);
        }
      }
    }

    const error = new Error('Replay media buffer was closed');
    this.activeOperation?.reject(error);
    for (const operation of this.operations) {
      operation.reject(error);
    }
    this.activeOperation = null;
    this.operations.length = 0;
    this.video.removeAttribute('src');
    this.video.load();
    URL.revokeObjectURL(this.objectUrl);
  }

  private readonly handleSourceOpen = (): void => {
    if (this.states.has('destroyed')) return;

    try {
      this.sourceBuffer = this.mediaSource.addSourceBuffer(this.mimeType);
      if (this.sourceBuffer.mode === 'segments') {
        try {
          this.sourceBuffer.mode = 'sequence';
        } catch (error) {
          console.info('[Replay] SourceBuffer sequence mode unavailable; using media timestamps:', error);
        }
      }
      this.mediaSource.duration = Number.POSITIVE_INFINITY;
      this.sourceBuffer.addEventListener('updateend', this.handleUpdateEnd);
      this.sourceBuffer.addEventListener('error', this.handleSourceBufferError);
      this.processNext();
    } catch (error) {
      let sourceError = new Error(`Cannot create a SourceBuffer for ${this.mimeType}`);
      if (error instanceof Error) sourceError = error;
      this.fail(sourceError);
    }
  };

  private readonly handleUpdateEnd = (): void => {
    const completed = this.activeOperation;
    this.activeOperation = null;
    completed?.resolve();
    this.reportRange();
    this.queueTrim();
    this.processNext();
  };

  private readonly handleSourceBufferError = (): void => {
    this.fail(new Error(`The browser rejected ${this.mimeType} replay data`));
  };

  private processNext(): void {
    const sourceBuffer = this.sourceBuffer;
    if (!sourceBuffer || sourceBuffer.updating || this.activeOperation || this.isReadingBlob) return;
    if (this.states.has('destroyed') || this.states.has('failed')) return;

    const operation = this.operations.shift();
    if (!operation) return;
    this.activeOperation = operation;

    if (operation.kind === 'remove') {
      try {
        sourceBuffer.remove(operation.start, operation.end);
      } catch (error) {
        let removeError = new Error('Failed to trim replay media');
        if (error instanceof Error) removeError = error;
        this.activeOperation = null;
        operation.reject(removeError);
        this.fail(removeError);
      }
      return;
    }

    this.isReadingBlob = true;
    operation.blob
      .arrayBuffer()
      .then((bytes) => {
        this.isReadingBlob = false;
        if (this.states.has('destroyed') || this.states.has('failed')) return;
        try {
          sourceBuffer.appendBuffer(bytes);
        } catch (error) {
          let appendError = new Error('Failed to append replay media');
          if (error instanceof Error) appendError = error;
          if (appendError.name === 'QuotaExceededError' && this.retryAfterStoragePressure(operation)) {
            return;
          }
          this.activeOperation = null;
          operation.reject(appendError);
          this.fail(appendError);
        }
      })
      .catch((error) => {
        let readError = new Error('Failed to read replay media');
        if (error instanceof Error) readError = error;
        this.isReadingBlob = false;
        this.activeOperation = null;
        operation.reject(readError);
        this.fail(readError);
      });
  }

  private retryAfterStoragePressure(operation: AppendOperation): boolean {
    const sourceBuffer = this.sourceBuffer;
    const nextWindowSeconds = this.onStoragePressure();
    if (!sourceBuffer || nextWindowSeconds == null) return false;

    const range = readBufferedRange(sourceBuffer);
    if (!range) return false;

    const cutoff = range.end - nextWindowSeconds;
    if (cutoff <= range.start) return false;

    this.windowSeconds = nextWindowSeconds;
    this.activeOperation = null;
    this.operations.unshift(operation);
    this.operations.unshift({
      kind: 'remove',
      start: 0,
      end: cutoff,
      resolve: () => undefined,
      reject: (error) => console.warn('[Replay] Failed to recover from storage pressure:', error),
    });
    this.processNext();

    return true;
  }

  private queueTrim(): void {
    const sourceBuffer = this.sourceBuffer;
    if (!sourceBuffer || this.windowSeconds <= 0 || this.states.has('destroyed') || this.states.has('failed')) return;

    const range = readBufferedRange(sourceBuffer);
    if (!range) return;

    const cutoff = range.end - this.windowSeconds;
    if (cutoff <= range.start + 0.25) return;

    const hasPendingRemove =
      this.activeOperation?.kind === 'remove' || this.operations.some((item) => item.kind === 'remove');
    if (hasPendingRemove) return;

    this.operations.unshift({
      kind: 'remove',
      start: 0,
      end: cutoff,
      resolve: () => undefined,
      reject: (error) => console.warn('[Replay] Failed to trim buffered media:', error),
    });
    this.processNext();
  }

  private reportRange(): void {
    const range = this.sourceBuffer ? readBufferedRange(this.sourceBuffer) : null;
    if (range && 'setLiveSeekableRange' in this.mediaSource) {
      try {
        this.mediaSource.setLiveSeekableRange(range.start, range.end);
      } catch (error) {
        console.debug('[Replay] Could not update the live seekable range:', error);
      }
    }
    this.onRangeChange(range);
  }

  private fail(error: Error): void {
    if (this.states.has('failed') || this.states.has('destroyed')) return;

    this.states.add('failed');
    this.activeOperation?.reject(error);
    for (const operation of this.operations) {
      operation.reject(error);
    }
    this.activeOperation = null;
    this.operations.length = 0;
    this.onFailure(error);
  }
}

export type { BufferedRange };
