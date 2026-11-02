import type { AudioApp } from '@slopcast/shared-types';

export interface CaptureContext {
  readonly de: 'unknown' | 'kde' | 'gnome';
  readonly sourceType: 'monitor' | 'window' | 'region' | 'unknown';
  readonly mediaName: string | null;
  readonly videoNodeCount: number;
  readonly app: AudioApp | null;
  readonly screencastNodeId: number | null;
  readonly highestSerial: number | null;
  readonly portalProps: Record<string, string> | null;
  readonly windowPid: number | null;
  readonly windowCaption: string | null;
}

export interface AppConfig {
  readonly apiEndpoint: string;
  readonly livekitUrl: string;
}

export interface PlatformInfo {
  readonly platform: string;
  readonly isWayland: boolean;
  readonly desktopEnvironment: string;
  readonly videoCaptureAvailable: boolean;
}

export interface WindowButtonIcons {
  readonly minimize: string | null;
  readonly maximize: string | null;
  readonly restore: string | null;
  readonly close: string | null;
}

export type CaptureSourceKind = 'screen' | 'window';

export interface CaptureSourceInfo {
  readonly id: number;
  readonly title: string;
  readonly displayId: number;
  readonly kind: CaptureSourceKind;
}

export interface CaptureSourceSelection {
  readonly kind: CaptureSourceKind;
  readonly id: number;
}

export type CaptureStage = 'idle' | 'previewing' | 'live';

export interface CaptureStartResult {
  readonly ok: boolean;
  readonly nodeId: number | null;
  readonly videoEnabled: boolean;
  readonly error?: string | null;
}

export interface DesktopCaptureConfig {
  readonly fps: number;
  readonly width: number;
  readonly height: number;
  readonly videoCodec?: string;
  readonly maxBitrate?: number;
  readonly autoBitrate?: boolean;
}

export interface PreviewFrame {
  readonly data: Uint8Array;
  readonly ptsUs: number;
  readonly width: number;
  readonly height: number;
}

export interface NativeCodecInfo {
  readonly codec: string;
  readonly label: string;
  readonly hardware: boolean;
}

export interface NativeTelemetry {
  readonly videoCodec: string | null;
  readonly encoderImplementation: string | null;
  readonly videoBytesSent: number | null;
  readonly videoPacketsSent: number | null;
  readonly videoPacketsLost: number | null;
  readonly videoFramesEncoded: number | null;
  readonly videoFramesSubmitted: number | null;
  readonly videoWidth: number | null;
  readonly videoHeight: number | null;
  readonly videoAppsrcInput: number | null;
  readonly videoAppsrcOutput: number | null;
  readonly videoAppsrcDropped: number | null;
  readonly videoAppsrcLevelBuffers: number | null;
  readonly videoAppsrcLevelBytes: number | null;
  readonly videoAppsrcLevelTime: number | null;
  readonly audioCodec: string | null;
  readonly audioBytesSent: number | null;
  readonly audioPacketsSent: number | null;
  readonly audioPacketsLost: number | null;
  readonly rttMs: number | null;
  readonly timestampMs: number | null;
}

export interface DesktopCaptureStats {
  readonly framesDequeued: number;
  readonly framesPushed: number;
  readonly framesDropped: number;
  readonly captureErrors: number;
  readonly previewFramesSent: number;
  readonly keepaliveAttempted: number;
  readonly keepalivePushed: number;
  readonly keepaliveDropped: number;
  readonly lastWidth: number;
  readonly lastHeight: number;
  readonly pacerPushes: number;
  readonly pacerPops: number;
  readonly pacerDrops: number;
  readonly pacerDepth: number;
  readonly pacerMaxDepth: number;
  readonly cursorFrames: number;
  readonly cursorMissing: boolean;
}

export interface GpuInfo {
  readonly eglVendor: string | null;
  readonly glRenderer: string | null;
  readonly glVersion: string | null;
  readonly softwareRasterizer: boolean;
}
