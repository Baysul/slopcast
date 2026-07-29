export interface StreamTelemetry {
  live: boolean;
  updatedAt: number;
  videoCodec: string | null;
  videoEncoder: string | null;
  width: number | null;
  height: number | null;
  frameRate: number | null;
  targetFrameRate: number | null;
  videoBitrate: number | null;
  audioCodec: string | null;
  audioBitrate: number | null;
  hasAudio: boolean;
  packetLossPct: number | null;
  roundTripTimeMs: number | null;
  bitrateHistory: number[];
  elapsedMs: number;
  spectatorCount: number;
}

export const idleTelemetry = (): StreamTelemetry => ({
  live: false,
  updatedAt: 0,
  videoCodec: null,
  videoEncoder: null,
  width: null,
  height: null,
  frameRate: null,
  targetFrameRate: null,
  videoBitrate: null,
  audioCodec: null,
  audioBitrate: null,
  hasAudio: false,
  packetLossPct: null,
  roundTripTimeMs: null,
  bitrateHistory: [],
  elapsedMs: 0,
  spectatorCount: 0,
});

export const fmtDuration = (ms: number): string => {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const pad = (n: number) => n.toString().padStart(2, '0');
  return `${pad(h)}:${pad(m)}:${pad(s)}`;
};
