import type { PreviewFrame } from '../types';

const HEADER_BYTES = 16;
const TIMESTAMP_OFFSET = 0;
const WIDTH_OFFSET = 8;
const HEIGHT_OFFSET = 12;
const BYTES_PER_PIXEL = 4;
const MIN_DIMENSION = 1;
const NO_CONTENT = 204;

const parsePreviewPayload = (payload: ArrayBuffer): PreviewFrame | null => {
  if (payload.byteLength <= HEADER_BYTES) {
    return null;
  }
  const view = new DataView(payload);
  const ptsUs = Number(view.getBigUint64(TIMESTAMP_OFFSET, true));
  const width = view.getUint32(WIDTH_OFFSET, true);
  const height = view.getUint32(HEIGHT_OFFSET, true);
  if (
    width < MIN_DIMENSION ||
    height < MIN_DIMENSION ||
    payload.byteLength !== HEADER_BYTES + width * height * BYTES_PER_PIXEL
  ) {
    return null;
  }
  return { data: new Uint8Array(payload, HEADER_BYTES), height, ptsUs, width };
};

/** Poll only during capture; serialize requests and reject frames after cleanup. */
export const startPreviewPolling = (renderFrame: (frame: PreviewFrame) => void): (() => void) => {
  const controller = new AbortController();
  const { signal } = controller;
  const isStopped = (): boolean => signal.aborted;
  let lastPts: number | null = null;
  let animationFrame: number | null = null;

  const poll = async (): Promise<void> => {
    if (isStopped()) {
      return;
    }
    try {
      let query = '';
      if (lastPts !== null) {
        query = `?after=${lastPts}`;
      }
      const response = await fetch(`http://frame.localhost/frame.bin${query}`, { cache: 'no-store', signal });
      if (response.ok && response.status !== NO_CONTENT && !isStopped()) {
        const frame = parsePreviewPayload(await response.arrayBuffer());
        if (frame && frame.ptsUs !== lastPts && !isStopped()) {
          lastPts = frame.ptsUs;
          renderFrame(frame);
        }
      }
    } catch (error) {
      if (!isStopped()) {
        console.debug('[preview] frame fetch failed:', error);
      }
    }
    if (!isStopped()) {
      animationFrame = requestAnimationFrame(() => void poll());
    }
  };

  animationFrame = requestAnimationFrame(() => void poll());
  return () => {
    controller.abort();
    if (animationFrame !== null) {
      cancelAnimationFrame(animationFrame);
    }
  };
};
