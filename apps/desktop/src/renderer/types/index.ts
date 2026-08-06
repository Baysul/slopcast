import type { AudioApp } from '@slopcast/shared-types';

/// Mirrors `CaptureContextDto` in `apps/desktop/src-tauri/src/dto.rs`: the
/// Wayland video-capture introspection returned by both `get_capture_context`
/// (cached) and `inspect_capture_context` (fresh).
export interface CaptureContext {
  de: 'unknown' | 'kde' | 'gnome';
  sourceType: 'monitor' | 'window' | 'region' | 'unknown';
  mediaName: string | null;
  videoNodeCount: number;
  app: AudioApp | null;
  screencastNodeId: number | null;
  /// `object.serial` of the newest `kwin-screencast-*` node, snapshotted
  /// before the portal dialog is triggered.
  highestSerial: number | null;
  portalProps: Record<string, string> | null;
  windowPid: number | null;
  windowCaption: string | null;
}

/// The subset of the app config exposed by the `get_app_config` command.
export interface AppConfig {
  apiEndpoint: string;
  livekitUrl: string;
}

export interface PlatformInfo {
  platform: string;
  isWayland: boolean;
}

/// Live capture-stage state machine for the pre-roll flow (MIGRATION §9.2):
/// preview capture runs before the track is published, and "Go Live" is the
/// primary action while previewing.
export type CaptureStage = 'idle' | 'previewing' | 'live';

/// Result of `start_native_capture`: `{ ok, nodeId, videoEnabled }`.
export interface CaptureStartResult {
  ok: boolean;
  nodeId: number | null;
  videoEnabled: boolean;
  error?: string | null;
}

/// Encoder configuration for `start_native_capture`, `update_native_video`
/// and `go_live` (mirrors `CaptureConfig` in native-livekit, camelCase).
export interface DesktopCaptureConfig {
  fps: number;
  width: number;
  height: number;
  videoCodec?: string;
  maxBitrate?: number;
}

/// One preview frame shipped over the preview channel: JPEG bytes encoded
/// natively by libjpeg-turbo (frame dimensions come from the decoded
/// bitmap), plus the native emission timestamp used for latency metrics.
export interface PreviewFrame {
  data: ArrayBuffer;
  ptsUs: number;
}

/// A codec the native encoder stack (bundled libwebrtc) can encode with, as
/// reported by `get_native_supported_codecs`. The renderer must never read
/// the webview's `RTCRtpSender.getCapabilities` — that stack is not used for
/// encoding.
export interface NativeCodecInfo {
  codec: string;
  label: string;
  hardware: boolean;
}

/// Cumulative libwebrtc counters reported by `get_native_telemetry`; deltas
/// are computed renderer-side exactly like the old `getStats()` path.
export interface NativeTelemetry {
  videoCodec: string | null;
  /** Actual encoder used, e.g. "VAAPI H264 Encoder" (hardware) vs "OpenH264"
   * (software) — from the outbound-rtp `encoderImplementation` stat. */
  encoderImplementation: string | null;
  videoBytesSent: number | null;
  videoPacketsSent: number | null;
  videoPacketsLost: number | null;
  videoFramesSent: number | null;
  videoWidth: number | null;
  videoHeight: number | null;
  audioCodec: string | null;
  audioBytesSent: number | null;
  audioPacketsSent: number | null;
  audioPacketsLost: number | null;
  rttMs: number | null;
  timestampMs: number | null;
}

/// Per-stage desktop-capture counters from `get_video_capture_stats`.
export interface DesktopCaptureStats {
  framesDequeued: number;
  framesPushed: number;
  framesDropped: number;
  captureErrors: number;
  /// JPEG preview frames emitted via the preview callback (encoded
  /// natively by libjpeg-turbo).
  previewFramesSent: number;
  lastWidth: number;
  lastHeight: number;
}

/// EGL probe output from `probe_gpu_info` (D5).
export interface GpuInfo {
  eglVendor: string | null;
  glRenderer: string | null;
  glVersion: string | null;
  softwareRasterizer: boolean;
}
