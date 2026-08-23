import type { AudioApp } from '@slopcast/shared-types';

export interface CaptureContext {
  de: 'unknown' | 'kde' | 'gnome';
  sourceType: 'monitor' | 'window' | 'region' | 'unknown';
  mediaName: string | null;
  videoNodeCount: number;
  app: AudioApp | null;
  screencastNodeId: number | null;
  highestSerial: number | null;
  portalProps: Record<string, string> | null;
  windowPid: number | null;
  windowCaption: string | null;
}

export interface AppConfig {
  apiEndpoint: string;
  livekitUrl: string;
}

export interface PlatformInfo {
  platform: string;
  isWayland: boolean;
  videoCaptureAvailable: boolean;
}

export type CaptureSourceKind = 'screen' | 'window';

export interface CaptureSourceInfo {
  id: number;
  title: string;
  displayId: number;
  kind: CaptureSourceKind;
}

export interface CaptureSourceSelection {
  kind: CaptureSourceKind;
  id: number;
}

export type CaptureStage = 'idle' | 'previewing' | 'live';

export interface CaptureStartResult {
  ok: boolean;
  nodeId: number | null;
  videoEnabled: boolean;
  error?: string | null;
}

export interface DesktopCaptureConfig {
  fps: number;
  width: number;
  height: number;
  videoCodec?: string;
  maxBitrate?: number;
  autoBitrate?: boolean;
}

export interface PreviewFrame {
  data: Uint8Array;
  ptsUs: number;
  width: number;
  height: number;
}

export interface NativeCodecInfo {
  codec: string;
  label: string;
  hardware: boolean;
}

export interface NativeTelemetry {
  videoCodec: string | null;
  encoderImplementation: string | null;
  videoBytesSent: number | null;
  videoPacketsSent: number | null;
  videoPacketsLost: number | null;
  videoFramesEncoded: number | null;
  videoFramesSubmitted: number | null;
  videoWidth: number | null;
  videoHeight: number | null;
  videoAppsrcInput: number | null;
  videoAppsrcOutput: number | null;
  videoAppsrcDropped: number | null;
  videoAppsrcLevelBuffers: number | null;
  videoAppsrcLevelBytes: number | null;
  videoAppsrcLevelTime: number | null;
  audioCodec: string | null;
  audioBytesSent: number | null;
  audioPacketsSent: number | null;
  audioPacketsLost: number | null;
  rttMs: number | null;
  timestampMs: number | null;
}

export interface DesktopCaptureStats {
  framesDequeued: number;
  framesPushed: number;
  framesDropped: number;
  captureErrors: number;
  previewFramesSent: number;
  keepaliveAttempted: number;
  keepalivePushed: number;
  keepaliveDropped: number;
  lastWidth: number;
  lastHeight: number;
  pacerPushes: number;
  pacerPops: number;
  pacerDrops: number;
  pacerDepth: number;
  pacerMaxDepth: number;
}

export interface GpuInfo {
  eglVendor: string | null;
  glRenderer: string | null;
  glVersion: string | null;
  softwareRasterizer: boolean;
}
