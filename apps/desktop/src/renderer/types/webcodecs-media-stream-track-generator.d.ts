declare class MediaStreamTrackGenerator extends MediaStreamTrack {
  constructor(options: Readonly<{ kind: 'audio' | 'video' }>);
  readonly writable: WritableStream<AudioData | VideoFrame>;
}
