import { type BufferedRange, ReplayMediaBuffer } from './replay-media-buffer';
import { createBrowserReplayOwnership, type ReplayOwnership, type ReplayOwnershipState } from './replay-ownership';
import { ReplayStore } from './replay-store';

export const REPLAY_DEFAULT_SECONDS = 120;
export const REPLAY_MAX_SECONDS = 300;
export const REPLAY_STEP_SECONDS = 30;
export const REPLAY_LIVE_TOLERANCE_SECONDS = 2;

const REPLAY_PREFERENCE_KEY = 'slopcast.replayWindowSeconds';
const RECORDING_CHUNK_MS = 2000;
const RECORDING_CHUNK_SECONDS = RECORDING_CHUNK_MS / 1000;
const THUMBNAIL_INTERVAL_SECONDS = 5;
const THUMBNAIL_WIDTH = 256;
const THUMBNAIL_HEIGHT = 144;
const THUMBNAIL_QUALITY = 0.75;

export type ReplayAvailability = 'checking' | 'available' | 'blocked' | 'unavailable';
export type ReplayMode = 'live' | 'replay';

export interface ReplayThumbnail {
  mediaTime: number;
  url: string;
}

export interface ViewerReplaySnapshot {
  availability: ReplayAvailability;
  unavailableReason: string | null;
  mode: ReplayMode;
  windowSeconds: number;
  effectiveWindowSeconds: number;
  range: BufferedRange | null;
  position: number;
  isShareEnded: boolean;
  limitationReason: string | null;
  announcement: string | null;
  preview: ReplayThumbnail | null;
}

const sanitizeWindowSeconds = (value: number): number => {
  if (!Number.isFinite(value) || value <= 0) return 0;
  const stepped = Math.round(value / REPLAY_STEP_SECONDS) * REPLAY_STEP_SECONDS;
  return Math.min(REPLAY_MAX_SECONDS, Math.max(REPLAY_STEP_SECONDS, stepped));
};

export const readReplayWindowSeconds = (): number => {
  try {
    const stored = window.localStorage.getItem(REPLAY_PREFERENCE_KEY);
    if (stored == null) return REPLAY_DEFAULT_SECONDS;

    const windowSeconds = sanitizeWindowSeconds(Number(stored));
    return windowSeconds > 0 ? windowSeconds : REPLAY_DEFAULT_SECONDS;
  } catch (error) {
    console.info('[Replay] Saved replay preference is unavailable; using two minutes:', error);
    return REPLAY_DEFAULT_SECONDS;
  }
};

const saveReplayWindowSeconds = (value: number): void => {
  if (value <= 0) return;

  try {
    window.localStorage.setItem(REPLAY_PREFERENCE_KEY, String(value));
  } catch (error) {
    console.info('[Replay] Replay preference could not be saved:', error);
  }
};

const recordingMimeTypes = (hasAudio: boolean): string[] => {
  if (hasAudio) {
    return ['video/webm;codecs=vp8,opus', 'video/webm;codecs=vp9,opus', 'video/mp4;codecs=avc1.42E01E,mp4a.40.2'];
  }

  return ['video/webm;codecs=vp8', 'video/webm;codecs=vp9', 'video/mp4;codecs=avc1.42E01E'];
};

const findReplayMimeType = (stream: MediaStream): string | null => {
  const hasAudio = stream.getAudioTracks().length > 0;
  for (const mimeType of recordingMimeTypes(hasAudio)) {
    if (MediaRecorder.isTypeSupported(mimeType) && MediaSource.isTypeSupported(mimeType)) {
      return mimeType;
    }
  }

  return null;
};

const replaySupportFailure = (stream: MediaStream): string | null => {
  if (!('MediaRecorder' in window)) return 'MediaRecorder is unavailable';
  if (!('MediaSource' in window)) return 'MediaSource is unavailable';
  if (!('indexedDB' in window)) return 'IndexedDB is unavailable';
  if (!findReplayMimeType(stream)) return 'No recording format is supported by both MediaRecorder and MediaSource';

  return null;
};

const parseStorageWriteFailure = (cause: unknown): 'quota' | Error => {
  if (cause instanceof DOMException && cause.name === 'QuotaExceededError') return 'quota';
  if (cause instanceof Error) return cause;

  return new Error('Browser storage rejected a replay chunk');
};

const createThumbnailBlob = async (video: HTMLVideoElement): Promise<Blob> => {
  if (video.videoWidth === 0 || video.videoHeight === 0) {
    throw new Error('The live video has no decoded frame for a replay thumbnail');
  }

  const scale = Math.min(THUMBNAIL_WIDTH / video.videoWidth, THUMBNAIL_HEIGHT / video.videoHeight);
  const width = video.videoWidth * scale;
  const height = video.videoHeight * scale;
  const x = (THUMBNAIL_WIDTH - width) / 2;
  const y = (THUMBNAIL_HEIGHT - height) / 2;

  if ('OffscreenCanvas' in window) {
    const canvas = new OffscreenCanvas(THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT);
    const context = canvas.getContext('2d', { alpha: false });
    if (!context) throw new Error('The browser could not create a thumbnail canvas');

    context.imageSmoothingEnabled = true;
    context.imageSmoothingQuality = 'high';
    context.fillStyle = '#000';
    context.fillRect(0, 0, THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT);
    context.drawImage(video, x, y, width, height);

    return canvas.convertToBlob({ type: 'image/jpeg', quality: THUMBNAIL_QUALITY });
  }

  const canvas = document.createElement('canvas');
  canvas.width = THUMBNAIL_WIDTH;
  canvas.height = THUMBNAIL_HEIGHT;
  const context = canvas.getContext('2d', { alpha: false });
  if (!context) throw new Error('The browser could not create a thumbnail canvas');

  context.imageSmoothingEnabled = true;
  context.imageSmoothingQuality = 'high';
  context.fillStyle = '#000';
  context.fillRect(0, 0, THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT);
  context.drawImage(video, x, y, width, height);

  return new Promise((resolve, reject) => {
    canvas.toBlob(
      (blob) => {
        if (blob) {
          resolve(blob);
          return;
        }
        reject(new Error('The browser could not encode a replay thumbnail'));
      },
      'image/jpeg',
      THUMBNAIL_QUALITY,
    );
  });
};

const createInitialSnapshot = (windowSeconds: number): ViewerReplaySnapshot => ({
  availability: 'checking',
  unavailableReason: null,
  mode: 'live',
  windowSeconds,
  effectiveWindowSeconds: windowSeconds,
  range: null,
  position: 0,
  isShareEnded: false,
  limitationReason: null,
  announcement: null,
  preview: null,
});

export class ViewerReplay {
  private snapshot: ViewerReplaySnapshot;
  private stream: MediaStream | null = null;
  private recorder: MediaRecorder | null = null;
  private mediaBuffer: ReplayMediaBuffer | null = null;
  private store: ReplayStore | null = null;
  private thumbnailEpoch = 0;
  private thumbnailCaptureEpoch: number | null = null;
  private thumbnailCaptureFailed = false;
  private lastThumbnailAt = 0;
  private thumbnails: ReplayThumbnail[] = [];
  private writeQueue: Promise<void> = Promise.resolve();
  private sequence = 0;
  private previousChunkEnd = 0;
  private generation = 0;
  private readonly states = new Set<'destroyed' | 'ending'>();
  private readonly ownership: ReplayOwnership;

  constructor(
    private readonly replayVideo: HTMLVideoElement,
    private readonly thumbnailVideo: HTMLVideoElement,
    windowSeconds: number,
    private readonly onChange: (snapshot: ViewerReplaySnapshot) => void,
  ) {
    this.snapshot = createInitialSnapshot(sanitizeWindowSeconds(windowSeconds));
    this.ownership = createBrowserReplayOwnership(this.handleOwnershipChange);
    this.replayVideo.addEventListener('timeupdate', this.handleReplayTimeUpdate);
    this.replayVideo.addEventListener('ended', this.handleReplayEnded);
  }

  getSnapshot(): ViewerReplaySnapshot {
    return this.snapshot;
  }

  async startShare(stream: MediaStream): Promise<void> {
    const generation = ++this.generation;
    this.stream = stream;
    this.states.delete('ending');
    this.update({
      availability: 'checking',
      unavailableReason: null,
      mode: 'live',
      effectiveWindowSeconds: this.snapshot.windowSeconds,
      range: null,
      position: 0,
      isShareEnded: false,
      limitationReason: null,
      announcement: null,
      preview: null,
    });

    await this.clearSession();
    if (this.states.has('destroyed') || generation !== this.generation) return;
    this.stream = stream;

    const failure = replaySupportFailure(stream);
    if (failure) {
      this.markUnavailable(failure);
      return;
    }
    if (this.snapshot.windowSeconds === 0) {
      this.update({ availability: 'available', unavailableReason: null });
      return;
    }

    const ownershipState = this.ownership.getState();
    if (ownershipState.status === 'unsupported' || ownershipState.status === 'failed') {
      this.markUnavailable(ownershipState.reason);
      return;
    }
    if (ownershipState.status === 'owned') {
      this.update({ availability: 'available', unavailableReason: null });
      await this.beginRecording(generation);
      return;
    }

    this.ownership.claim();
  }

  endShare(): void {
    if (this.states.has('ending') || this.snapshot.isShareEnded) return;

    this.states.add('ending');
    this.stream = null;

    if (this.recorder?.state === 'recording') {
      this.recorder.stop();
      return;
    }

    this.finishShare();
  }

  setWindowSeconds(value: number): void {
    const windowSeconds = sanitizeWindowSeconds(value);
    saveReplayWindowSeconds(windowSeconds);
    this.update({
      windowSeconds,
      effectiveWindowSeconds: windowSeconds,
      limitationReason: null,
    });

    if (windowSeconds === 0) {
      this.stopRecording();
      this.mediaBuffer?.destroy();
      this.mediaBuffer = null;
      this.clearStoredMedia();
      this.resetThumbnailCapture();
      this.releaseThumbnails();
      this.ownership.release();
      this.update({
        availability: 'available',
        unavailableReason: null,
        mode: 'live',
        range: null,
        position: 0,
        preview: null,
      });
      return;
    }

    this.mediaBuffer?.setWindowSeconds(windowSeconds);
    this.pruneToWindow();
    if (this.stream && !this.recorder) {
      const ownershipState = this.ownership.getState();
      if (ownershipState.status === 'owned') {
        const generation = this.generation;
        this.beginRecording(generation).catch((error) => {
          this.markUnavailable(error instanceof Error ? error.message : 'Replay recording failed');
        });
        return;
      }
      this.ownership.claim();
    }
  }

  seek(mediaTime: number, shouldPlay: boolean): void {
    const range = this.snapshot.range;
    if (!range) return;

    const clampedTime = Math.min(range.end, Math.max(range.start, mediaTime));
    this.replayVideo.currentTime = clampedTime;
    this.update({ mode: 'replay', position: clampedTime, announcement: null });
    if (shouldPlay) {
      this.playReplay();
      return;
    }
    this.replayVideo.pause();
  }

  goLive(): void {
    const range = this.snapshot.range;
    const position = range ? range.end : 0;

    this.replayVideo.pause();
    this.update({
      mode: 'live',
      position,
      announcement: null,
      preview: null,
    });
  }

  playReplay(): void {
    const range = this.snapshot.range;
    if (!range) return;

    if (this.replayVideo.currentTime < range.start) {
      this.replayVideo.currentTime = range.start;
      this.update({
        position: range.start,
        announcement: 'Replay window advanced to the oldest available moment',
      });
    }
    this.replayVideo.play().catch((error) => {
      console.warn('[Replay] Buffered playback could not start:', error);
    });
  }

  pauseReplay(): void {
    this.replayVideo.pause();
  }

  preview(mediaTime: number | null): void {
    if (mediaTime == null || this.thumbnails.length === 0) {
      if (this.snapshot.preview) this.update({ preview: null });
      return;
    }

    let nearest = this.thumbnails[0];
    for (const thumbnail of this.thumbnails) {
      if (!nearest || Math.abs(thumbnail.mediaTime - mediaTime) < Math.abs(nearest.mediaTime - mediaTime)) {
        nearest = thumbnail;
      }
    }
    if (this.snapshot.preview === nearest) return;

    this.update({ preview: nearest ?? null });
  }

  clearAnnouncement(): void {
    if (this.snapshot.announcement) {
      this.update({ announcement: null });
    }
  }

  destroy(): void {
    if (this.states.has('destroyed')) return;

    this.states.add('destroyed');
    this.generation += 1;
    this.stopRecording();
    this.mediaBuffer?.destroy();
    this.mediaBuffer = null;
    this.replayVideo.removeEventListener('timeupdate', this.handleReplayTimeUpdate);
    this.replayVideo.removeEventListener('ended', this.handleReplayEnded);
    this.resetThumbnailCapture();
    this.releaseThumbnails();
    this.clearStoredMedia();
    this.ownership.destroy();
  }

  private async beginRecording(generation: number): Promise<void> {
    const stream = this.stream;
    if (
      !stream ||
      this.states.has('destroyed') ||
      generation !== this.generation ||
      this.snapshot.windowSeconds === 0
    ) {
      return;
    }

    const mimeType = findReplayMimeType(stream);
    if (!mimeType) {
      this.markUnavailable('No recording format is supported by both MediaRecorder and MediaSource');
      return;
    }

    const sessionId = crypto.randomUUID();
    let store: ReplayStore;
    try {
      store = await ReplayStore.create(sessionId);
    } catch (error) {
      this.markUnavailable(
        `IndexedDB initialization failed: ${error instanceof Error ? error.message : String(error)}`,
      );
      return;
    }
    if (this.states.has('destroyed') || generation !== this.generation || this.stream !== stream) {
      await store.clear();
      store.close();
      return;
    }

    this.store = store;
    this.resetThumbnailCapture();
    this.mediaBuffer = new ReplayMediaBuffer(
      this.replayVideo,
      mimeType,
      this.snapshot.effectiveWindowSeconds,
      this.handleRangeChange,
      this.reduceEffectiveWindow,
      this.handleMediaBufferFailure,
    );

    try {
      this.recorder = new MediaRecorder(stream, { mimeType });
    } catch (error) {
      this.markUnavailable(
        `MediaRecorder initialization failed: ${error instanceof Error ? error.message : String(error)}`,
      );
      return;
    }
    this.recorder.addEventListener('dataavailable', this.handleRecordedData);
    this.recorder.addEventListener('error', this.handleRecorderError);
    this.recorder.addEventListener('stop', this.handleRecorderStop);
    this.recorder.start(RECORDING_CHUNK_MS);
  }

  private readonly handleRecordedData = (event: BlobEvent): void => {
    if (event.data.size === 0 || this.states.has('destroyed')) return;

    const blob = event.data;
    const sequence = this.sequence++;
    const generation = this.generation;
    this.writeQueue = this.writeQueue
      .then(async () => {
        if (generation !== this.generation) return;
        await this.appendAndStoreChunk(blob, sequence);
      })
      .catch((error) => {
        if (generation !== this.generation) return;
        this.markUnavailable(error instanceof Error ? error.message : 'Replay chunk processing failed');
      });
  };

  private readonly handleRecorderError = (event: Event): void => {
    // SAFETY: MediaRecorder error events carry the DOMException that stopped recording.
    const recorderError = (event as Event & { error?: DOMException }).error;
    const isTrackChange =
      recorderError?.name === 'InvalidModificationError' ||
      recorderError?.message.includes('Tracks in MediaStream') === true;
    if (isTrackChange) {
      this.states.add('ending');
      this.stream = null;
      return;
    }

    const detail = recorderError?.message ?? 'unknown recording error';
    this.markUnavailable(`MediaRecorder failed: ${detail}`);
  };

  private readonly handleRecorderStop = (): void => {
    this.writeQueue
      .then(() => {
        if (this.states.has('ending')) {
          this.finishShare();
        }
      })
      .catch((error) => {
        this.markUnavailable(error instanceof Error ? error.message : 'Replay finalization failed');
      });
  };

  private async appendAndStoreChunk(blob: Blob, sequence: number): Promise<void> {
    const mediaBuffer = this.mediaBuffer;
    const store = this.store;
    if (!mediaBuffer || !store) return;

    await mediaBuffer.append(blob);
    const range = mediaBuffer.getRange();
    if (!range) return;

    const startedAt = Math.max(range.start, this.previousChunkEnd);
    const endedAt = range.end;
    this.previousChunkEnd = endedAt;

    this.captureThumbnail(endedAt, range.start);
    await this.storeChunkWithFallback(store, { sequence, startedAt, endedAt, blob });
    await store.prune(Math.max(0, endedAt - this.snapshot.effectiveWindowSeconds));
    this.pruneThumbnails(range.start);
  }

  private async storeChunkWithFallback(
    store: ReplayStore,
    chunk: { sequence: number; startedAt: number; endedAt: number; blob: Blob },
  ): Promise<void> {
    try {
      await store.putChunk(chunk);
      return;
    } catch (error) {
      const failure = parseStorageWriteFailure(error);
      if (failure !== 'quota') throw failure;
    }

    let nextWindow = this.reduceEffectiveWindow();
    while (nextWindow != null) {
      await store.prune(Math.max(0, chunk.endedAt - nextWindow));
      try {
        await store.putChunk(chunk);
        return;
      } catch (error) {
        const failure = parseStorageWriteFailure(error);
        if (failure !== 'quota') throw failure;
        nextWindow = this.reduceEffectiveWindow();
      }
    }

    throw new Error('Browser storage cannot retain a replay chunk');
  }

  private readonly handleRangeChange = (range: BufferedRange | null): void => {
    let position = this.snapshot.position;
    if (this.snapshot.mode === 'live') {
      position = range ? range.end : 0;
    }
    if (
      range &&
      this.snapshot.mode === 'replay' &&
      !this.replayVideo.paused &&
      this.replayVideo.currentTime < range.start
    ) {
      this.replayVideo.currentTime = range.start;
      this.update({
        range,
        position: range.start,
        announcement: 'Replay window advanced to the oldest available moment',
      });
      return;
    }
    this.update({ range, position });
  };

  private readonly reduceEffectiveWindow = (): number | null => {
    const current = this.snapshot.effectiveWindowSeconds;
    if (current <= RECORDING_CHUNK_SECONDS) return null;

    const next = Math.max(RECORDING_CHUNK_SECONDS, current - REPLAY_STEP_SECONDS);
    this.update({
      effectiveWindowSeconds: next,
      limitationReason: `Storage pressure limited replay to ${Math.round(next)} seconds`,
    });
    this.mediaBuffer?.setWindowSeconds(next);

    return next;
  };

  private readonly handleMediaBufferFailure = (error: Error): void => {
    this.markUnavailable(`Seekable replay failed: ${error.message}`);
  };

  private readonly handleReplayTimeUpdate = (): void => {
    if (this.snapshot.mode !== 'replay') return;
    this.update({ position: this.replayVideo.currentTime });
  };

  private readonly handleReplayEnded = (): void => {
    if (this.snapshot.isShareEnded) {
      this.replayVideo.pause();
      return;
    }
    this.goLive();
  };

  private resetThumbnailCapture(): void {
    this.thumbnailEpoch += 1;
    this.thumbnailCaptureEpoch = null;
    this.thumbnailCaptureFailed = false;
    this.lastThumbnailAt = 0;
  }

  private captureThumbnail(mediaTime: number, cutoff: number): void {
    const video = this.thumbnailVideo;
    const thumbnailEpoch = this.thumbnailEpoch;
    if (this.thumbnailCaptureEpoch === thumbnailEpoch || this.thumbnailCaptureFailed) return;
    if (this.lastThumbnailAt > 0 && mediaTime - this.lastThumbnailAt < THUMBNAIL_INTERVAL_SECONDS) return;
    if (video.readyState < HTMLMediaElement.HAVE_CURRENT_DATA || video.videoWidth === 0 || video.videoHeight === 0) {
      return;
    }

    this.thumbnailCaptureEpoch = thumbnailEpoch;
    this.lastThumbnailAt = mediaTime;
    createThumbnailBlob(video)
      .then((blob) => {
        if (this.states.has('destroyed') || thumbnailEpoch !== this.thumbnailEpoch) return;

        this.thumbnails.push({ mediaTime, url: URL.createObjectURL(blob) });
        this.pruneThumbnails(cutoff);
      })
      .catch((error) => {
        if (this.states.has('destroyed') || thumbnailEpoch !== this.thumbnailEpoch) return;

        this.thumbnailCaptureFailed = true;
        console.info('[Replay] Thumbnail generation unavailable; using timestamp previews:', error);
      })
      .finally(() => {
        if (this.thumbnailCaptureEpoch === thumbnailEpoch) this.thumbnailCaptureEpoch = null;
      });
  }

  private pruneToWindow(): void {
    const range = this.snapshot.range;
    const store = this.store;
    if (!range || !store || this.snapshot.effectiveWindowSeconds <= 0) return;

    const cutoff = Math.max(0, range.end - this.snapshot.effectiveWindowSeconds);
    store.prune(cutoff).catch((error) => {
      console.warn('[Replay] Could not apply the shorter replay window:', error);
    });
    this.pruneThumbnails(cutoff);
  }

  private pruneThumbnails(cutoff: number): void {
    const retained: ReplayThumbnail[] = [];
    let shouldClearPreview = false;
    for (const thumbnail of this.thumbnails) {
      if (thumbnail.mediaTime < cutoff) {
        shouldClearPreview ||= this.snapshot.preview === thumbnail;
        URL.revokeObjectURL(thumbnail.url);
      } else {
        retained.push(thumbnail);
      }
    }
    this.thumbnails = retained;
    if (shouldClearPreview) this.update({ preview: null });
  }

  private finishShare(): void {
    this.states.delete('ending');
    const range = this.snapshot.range;
    if (!range) {
      this.ownership.release();
      this.update({ availability: 'available', isShareEnded: true, mode: 'live' });
      return;
    }

    const finalPosition = Math.max(range.start, range.end - 0.05);
    this.replayVideo.currentTime = finalPosition;
    this.replayVideo.pause();
    this.update({
      mode: 'replay',
      position: finalPosition,
      isShareEnded: true,
      announcement: 'Stream ended. Replay remains available.',
    });
  }

  private stopRecording(): void {
    const recorder = this.recorder;
    this.recorder = null;
    if (!recorder) return;

    recorder.removeEventListener('dataavailable', this.handleRecordedData);
    recorder.removeEventListener('error', this.handleRecorderError);
    recorder.removeEventListener('stop', this.handleRecorderStop);
    if (recorder.state !== 'inactive') {
      recorder.stop();
    }
  }

  private markUnavailable(reason: string): void {
    if (this.snapshot.availability === 'unavailable' && this.snapshot.unavailableReason === reason) return;

    console.info(`[Replay] Unavailable: ${reason}`);
    this.stopRecording();
    this.mediaBuffer?.destroy();
    this.mediaBuffer = null;
    this.resetThumbnailCapture();
    this.releaseThumbnails();
    this.clearStoredMedia();
    this.ownership.release();
    this.update({
      availability: 'unavailable',
      unavailableReason: reason,
      mode: 'live',
      range: null,
      position: 0,
      limitationReason: null,
      preview: null,
    });
  }

  private releaseThumbnails(): void {
    for (const thumbnail of this.thumbnails) {
      URL.revokeObjectURL(thumbnail.url);
    }
    this.thumbnails = [];
  }

  private clearStoredMedia(): void {
    const store = this.store;
    this.store = null;
    if (!store) return;

    store
      .clear()
      .catch((error) => console.warn('[Replay] Failed to clear temporary replay media:', error))
      .finally(() => store.close());
  }

  private readonly handleOwnershipChange = (state: ReplayOwnershipState): void => {
    if (this.states.has('destroyed')) return;
    if (state.status === 'waiting') {
      this.update({ availability: 'blocked', unavailableReason: null });
      return;
    }
    if (state.status === 'owned') {
      this.update({ availability: 'available', unavailableReason: null });
      if (!this.stream || this.snapshot.windowSeconds === 0 || this.recorder) return;

      const generation = this.generation;
      this.beginRecording(generation).catch((error) => {
        this.markUnavailable(error instanceof Error ? error.message : 'Replay recording failed');
      });
      return;
    }
    if (state.status === 'unsupported' || state.status === 'failed') {
      this.markUnavailable(state.reason);
    }
  };

  private async clearSession(): Promise<void> {
    this.stopRecording();
    this.mediaBuffer?.destroy();
    this.mediaBuffer = null;
    this.replayVideo.pause();
    this.resetThumbnailCapture();
    this.releaseThumbnails();

    const store = this.store;
    this.store = null;
    if (store) {
      try {
        await store.clear();
      } catch (error) {
        console.warn('[Replay] Failed to clear the previous share interval:', error);
      } finally {
        store.close();
      }
    }

    this.sequence = 0;
    this.previousChunkEnd = 0;
    this.writeQueue = Promise.resolve();
  }

  private update(changes: Partial<ViewerReplaySnapshot>): void {
    this.snapshot = { ...this.snapshot, ...changes };
    this.onChange(this.snapshot);
  }
}
