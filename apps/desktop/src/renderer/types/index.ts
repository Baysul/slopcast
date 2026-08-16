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
  /// Whether a real video capture route exists: Wayland (portal) on Linux,
  /// WGC on Windows. X11/macOS degrade to audio-only.
  videoCaptureAvailable: boolean;
}

/// A capturable screen or window in the Windows WGC source picker.
export type CaptureSourceKind = 'screen' | 'window';

/// One capturable source as reported by `get_capture_sources`.
export interface CaptureSourceInfo {
  id: number;
  title: string;
  displayId: number;
  kind: CaptureSourceKind;
}

/// The picker's selection, passed to `start_native_capture` /
/// `start_capture_preview` / `go_live` on Windows (ignored on Linux, where
/// the portal picker decides).
export interface CaptureSourceSelection {
  kind: CaptureSourceKind;
  id: number;
}

/// Live capture-stage state machine for the pre-roll flow:
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
  /** Whether the native publisher's loss-driven rate controller may step
   * the encoder below `maxBitrate`. `false` (manual) pins it at the
   * configured ceiling. */
  autoBitrate?: boolean;
}

/// One preview frame shipped over the preview channel: tightly packed BGRA
/// rows (native DMA-BUF readback byte order — the renderer uploads them to a
/// GPU texture as-is, no decode), plus the frame dimensions and the native
/// emission timestamp used for latency metrics. The 16-byte little-endian
/// channel header (`u64 pts_us`, `u32 width`, `u32 height`) is already
/// stripped by the channel callback.
export interface PreviewFrame {
  /** Zero-copy view over the channel payload (the 16-byte header stripped);
   * the IPC buffer is fresh per message, so the view stays valid for the
   * lifetime of this frame object. */
  data: Uint8Array;
  ptsUs: number;
  width: number;
  height: number;
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
  /** Encoded-frame count — on the Linux GStreamer branch measured after
   * h264parse (the true encoder throughput); the renderer derives fps here. */
  videoFramesEncoded: number | null;
  /** Linux GStreamer branch only: frames pushed into the video appsrc.
   * Shortfall vs. `videoFramesEncoded` = backpressure drops (also visible
   * in `videoAppsrcDropped`). */
  videoFramesSubmitted: number | null;
  videoWidth: number | null;
  videoHeight: number | null;
  /** Live video appsrc stats — `dropped` counts buffers the appsrc discarded
   * (leaky downstream on a full queue), the stutter diagnostic. */
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

/// Per-stage desktop-capture counters from `get_video_capture_stats`.
export interface DesktopCaptureStats {
  framesDequeued: number;
  framesPushed: number;
  framesDropped: number;
  captureErrors: number;
  /// JPEG preview frames emitted via the preview callback (encoded
  /// natively by libjpeg-turbo).
  previewFramesSent: number;
  keepaliveAttempted: number;
  keepalivePushed: number;
  keepaliveDropped: number;
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
