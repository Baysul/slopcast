export interface PcmAudioTrackResult {
  track: MediaStreamTrack;
  stop: () => void;
}

export function createPcmAudioTrack(): PcmAudioTrackResult | null {
  if (!window.electronAPI?.onAudioPcmData) {
    return null;
  }

  const audioCtx = new AudioContext({ sampleRate: 48000 });
  const destination = audioCtx.createMediaStreamDestination();

  const bufferSize = 2048;
  const scriptNode = audioCtx.createScriptProcessor(bufferSize, 0, 2);

  // Ring buffers with head/tail indices: shifting a plain array per sample is
  // O(n) on the audio thread and drops frames under load. Both indices reset
  // when drained, so the monotonic buffer never needs compaction.
  const MAX_QUEUE = 48000 * 2;
  const queueLeft = new Float32Array(MAX_QUEUE);
  const queueRight = new Float32Array(MAX_QUEUE);
  let head = 0;
  let tail = 0;

  const push = (l: number, r: number) => {
    if (tail < MAX_QUEUE) {
      queueLeft[tail] = l;
      queueRight[tail] = r;
      tail++;
    }
  };

  const pop = (count: number, outL: Float32Array, outR: Float32Array) => {
    for (let i = 0; i < count; i++) {
      outL[i] = queueLeft[head + i] ?? 0;
      outR[i] = queueRight[head + i] ?? 0;
    }
    head += count;
    if (head >= tail) {
      head = 0;
      tail = 0;
    }
  };

  const unsubscribe = window.electronAPI.onAudioPcmData((buffer: ArrayBuffer) => {
    const int16 = new Int16Array(buffer);
    const numFrames = int16.length / 2;
    for (let i = 0; i < numFrames; i++) {
      push((int16[i * 2] ?? 0) / 32768.0, (int16[i * 2 + 1] ?? 0) / 32768.0);
    }
  });

  scriptNode.onaudioprocess = (e: AudioProcessingEvent) => {
    const outL = e.outputBuffer.getChannelData(0);
    const outR = e.outputBuffer.getChannelData(1);
    const len = outL.length;
    const available = tail - head;
    const copyCount = Math.min(available, len);
    pop(copyCount, outL, outR);
    for (let i = copyCount; i < len; i++) {
      outL[i] = 0;
      outR[i] = 0;
    }
  };

  scriptNode.connect(destination);

  const track = destination.stream.getAudioTracks()[0];
  if (!track) {
    unsubscribe();
    scriptNode.disconnect();
    void audioCtx.close();
    return null;
  }

  const cleanup = () => {
    unsubscribe();
    scriptNode.disconnect();
    void audioCtx.close();
  };

  const originalStop = track.stop.bind(track);
  track.stop = () => {
    cleanup();
    originalStop();
  };

  return { track, stop: track.stop.bind(track) };
}
