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

  const queueLeft: number[] = [];
  const queueRight: number[] = [];
  const MAX_QUEUE = 48000 * 2;

  const unsubscribe = window.electronAPI.onAudioPcmData((buffer: ArrayBuffer) => {
    const int16 = new Int16Array(buffer);
    const numFrames = int16.length / 2;
    for (let i = 0; i < numFrames; i++) {
      const l = (int16[i * 2] ?? 0) / 32768.0;
      const r = (int16[i * 2 + 1] ?? 0) / 32768.0;
      if (queueLeft.length < MAX_QUEUE) {
        queueLeft.push(l);
        queueRight.push(r);
      }
    }
  });

  scriptNode.onaudioprocess = (e: AudioProcessingEvent) => {
    const outL = e.outputBuffer.getChannelData(0);
    const outR = e.outputBuffer.getChannelData(1);
    const len = outL.length;

    if (queueLeft.length >= len) {
      for (let i = 0; i < len; i++) {
        outL[i] = queueLeft.shift() ?? 0;
        outR[i] = queueRight.shift() ?? 0;
      }
    } else {
      const available = queueLeft.length;
      for (let i = 0; i < available; i++) {
        outL[i] = queueLeft.shift() ?? 0;
        outR[i] = queueRight.shift() ?? 0;
      }
      for (let i = available; i < len; i++) {
        outL[i] = 0;
        outR[i] = 0;
      }
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
