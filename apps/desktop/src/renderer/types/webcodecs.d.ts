declare class MediaStreamTrackGenerator extends MediaStreamTrack {
  constructor(options: { kind: 'audio' | 'video' });
  readonly writable: WritableStream<AudioData | VideoFrame>;
}

declare class AudioData {
  constructor(init: {
    format: 'u8' | 's16' | 's32' | 'f32' | 'u8-planar' | 's16-planar' | 's32-planar' | 'f32-planar';
    sampleRate: number;
    numberOfChannels: number;
    numberOfFrames: number;
    timestamp: number;
    data: ArrayBuffer;
  });
  readonly format: string;
  readonly sampleRate: number;
  readonly numberOfChannels: number;
  readonly numberOfFrames: number;
  readonly duration: number;
  readonly timestamp: number;
  close(): void;
}
