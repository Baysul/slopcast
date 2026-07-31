# Video File Streaming — Architecture Research Report

**Date:** 2026-07-30
**Status:** Research complete — recommends two-phase implementation starting with browser-native path

---

## 1. Feature Summary & Assumptions

**Goal:** Allow the desktop presenter to select a local video file (MP4, MKV, MOV, WebM, AVI) as a streaming source. The file is decoded, and its video + audio are published through the existing LiveKit/WebRTC pipeline. Spectators experience it as a normal screenshare.

**Assumptions made:**
- Video files are stored locally on the presenter's machine; no network/URL sources in scope.
- The presenter picks one file at a time (single-file, no playlist).
- Seeking (jump to timestamp), pause/resume, and loop are expected controls.
- The feature is desktop-only; the web spectator client needs no changes.
- Audio from the video file replaces (not mixes with) any other audio capture for the duration of the file share.
- Electron 43 (Chromium 136+) — all modern media APIs are available.

---

## 2. Current-State Findings

### 2.1 Source type abstraction does not exist

The codebase models exactly one video source: screen/window capture. The `canStartShare` check (line 1392 of `main.tsx`) is:

```ts
const canStartShare = !!roomCode && !isSharing && (isWayland || !!selectedSourceId);
```

There is no `VideoSourceType` enum or discriminated union anywhere in the pipeline. `captureVideoTrack()` (line 1134) has two hard-coded branches — Wayland portal vs. X11 desktop capturer — but no abstraction over *what kind* of source is producing the track.

**Implication:** Introducing a video-file source requires a source type discriminator, not just bolting on a third branch. This is the single largest architectural change.

### 2.2 The publishing pipeline is source-agnostic

Once a `MediaStreamTrack` is obtained, the LiveKit publish path (line 1274) works identically regardless of how the track was created:

```ts
await room.localParticipant.publishTrack(videoTrack, {
  source: Track.Source.ScreenShare,
  screenShareEncoding: undefined,
  simulcast: false,
});
```

This means any approach that produces a valid `MediaStreamTrack` (from `getDisplayMedia`, `captureStream()`, `MediaStreamTrackGenerator`, or `getUserMedia`) integrates into the existing publish pipeline without changes.

### 2.3 Audio capture has its own independent pipeline

Audio capture currently routes through the Rust native layer (PipeWire on Linux, WASAPI on Windows, CoreAudio on macOS). It is independently started/stopped/auto-resolved and published as a separate track. For video-file streaming, the file's audio track can either:
- Come bundled with the video track via `captureStream()` (bypassing the native audio pipeline entirely), or
- Be decoded and fed through the existing audio capture infrastructure.

### 2.4 WebCodecs type stubs already exist

`apps/desktop/src/renderer/types/webcodecs.d.ts` declares `MediaStreamTrackGenerator` and `AudioData` — the project anticipated needing low-level media frame access. This means the WebCodecs path (Option B below) is already partially scaffolded.

### 2.5 No file handling infrastructure exists

No `dialog.showOpenDialog`, no file path IPC, no `<input type="file">`. The desktop app currently handles zero user-facing file operations. Everything needed for video-file selection is new ground: IPC handler, preload bridge method, file dialog filtering, and path validation.

### 2.6 Renderer is monolithic

The source picker UI and all streaming logic live in `apps/desktop/src/renderer/main.tsx` (~1861 lines). Task 9 in the backlog calls out decomposing it. Adding video-file source selection before extraction compounds the monolith problem. The feature should extract source selection into its own component (e.g., `SourcePicker`) as part of implementation.

---

## 3. Options Considered

### Option A: `<video>` + `captureStream()`

Play the file in a hidden `<video>` element, call `video.captureStream()` to get a `MediaStream` with synced video + audio tracks, publish to LiveKit.

| Criterion | Assessment |
|---|---|
| **New dependencies** | None |
| **New Rust code** | None |
| **New IPC handlers** | 1 (file open dialog) |
| **Container support** | MP4, WebM, Ogg (Chromium's `<video>` set) |
| **Video codecs** | H.264, VP8, VP9, AV1 |
| **Audio codecs** | AAC, Opus, Vorbis |
| **H.265/HEVC** | Platform-dependent; works on Windows with HW decoder, typically not on Linux |
| **MKV / AVI / MOV** | Not supported by `<video>` — file would fail to play |
| **Audio path** | Bundled with video via `captureStream()` — no separate audio capture needed |
| **Synced A/V** | ✅ Browser's media pipeline handles sync automatically |
| **Seeking** | ✅ `video.currentTime` assignment |
| **Looping** | ✅ `video.loop = true` |
| **Pause/resume** | ✅ `video.pause()` / `video.play()` |
| **Hardware decode** | ✅ Automatic (Chromium uses platform decoders) |
| **Frame-accurate timing** | ❌ Frame timing is the browser's, not controllable |
| **Frame-level processing** | ❌ No access to decoded frames |
| **Performance (1080p60)** | Excellent — GPU decode, zero copy to encoder when same GPU |
| **Implementation cost** | ~30 lines of JS for the core; ~100 lines total with file picker + UI |

### Option B: WebCodecs + MediaStreamTrackGenerator

Use the WebCodecs `VideoDecoder`/`AudioDecoder` to decode the file, push `VideoFrame`/`AudioData` objects into a `MediaStreamTrackGenerator`, stream the resulting track.

| Criterion | Assessment |
|---|---|
| **New dependencies** | MP4/MKV demuxer (JS-side, e.g., mp4box.js, or Rust-side piped through napi-rs) |
| **New Rust code** | Possible (demuxer only, not decoder) |
| **Container support** | Anything the demuxer can parse |
| **Video codecs** | H.264, H.265, VP8, VP9, AV1 (Chromium's WebCodecs set) |
| **Audio codecs** | AAC, Opus, MP3, FLAC, Vorbis |
| **Audio path** | Must handle separately — audio decoded via `AudioDecoder` → `MediaStreamTrackGenerator` |
| **Synced A/V** | ⚠️ Must implement sync clock manually between video and audio decoder outputs |
| **Seeking** | Must restart decoder with new keyframe — non-trivial |
| **Hardware decode** | ✅ Automatic |
| **Frame-accurate timing** | ✅ Full control over frame presentation |
| **Frame-level processing** | ✅ `VideoFrame` objects available for processing |
| **Performance (1080p60)** | Good, but demuxing + decode plumbing adds overhead |
| **Implementation cost** | High: demuxer + decoder wiring + sync clock + seeking + A/V track management |

### Option C: Rust-side FFmpeg decode

Use `ffmpeg-next` (or `ez-ffmpeg`) in the native Rust layer to demux/decode, push raw frames and PCM audio through napi-rs to the renderer, then into WebRTC.

| Criterion | Assessment |
|---|---|
| **New dependencies** | `ffmpeg-next` crate + system FFmpeg (or bundled) |
| **New Rust code** | Substantial: demux/decode loops, frame delivery, audio resampling |
| **New IPC** | High-bandwidth frame transfer path (shared memory or serialized frames) |
| **Container support** | Everything FFmpeg supports (MP4, MKV, MOV, WebM, AVI, FLV, etc.) |
| **Video codecs** | Everything FFmpeg supports (H.264, H.265, VP8/9, AV1, ProRes, MPEG-2/4, etc.) |
| **Audio codecs** | Everything FFmpeg supports (AAC, Opus, MP3, FLAC, Vorbis, AC-3, DTS, etc.) |
| **Audio path** | Decoded in Rust; needs delivery path to renderer |
| **Synced A/V** | ⚠️ Must implement sync clock in Rust (mirrors OBS's `media-playback`) |
| **Seeking** | ✅ `av_seek_frame()` — full packet-level seeking |
| **Hardware decode** | ✅ Via FFmpeg hwaccel (VAAPI, NVDEC, VideoToolbox) |
| **Frame-level processing** | ✅ Raw frames available in Rust |
| **Performance (1080p60)** | Excellent — native FFmpeg is fast, but IPC frame transfer is the bottleneck |
| **Implementation cost** | Very high: multi-week effort; significant build system impact; per-platform FFmpeg linking |
| **Shipping complexity** | Must bundle or require system FFmpeg (~30–50 MB shared libraries) |

### Option Comparison Table

| Dimension | A: video + captureStream | B: WebCodecs + MSTG | C: Rust FFmpeg |
|---|---|---|---|
| **File format breadth** | Narrow (MP4/WebM/Ogg) | Medium (demuxer-limited) | Full |
| **H.265/HEVC** | Partial/platform | Yes | Yes |
| **AV sync** | Automatic | Manual | Manual |
| **Seeking** | Simple (currentTime) | Complex | Moderate |
| **New dependencies** | 0 | 1 JS demuxer lib | ~5 Rust crates + system FFmpeg |
| **Build system impact** | None | None | Per-platform FFmpeg linking |
| **New Rust code** | 0 lines | 0–100 lines (demuxer) | 500–1500 lines |
| **New JS code** | ~100 lines | ~300–500 lines | ~100–200 lines (IPC consumer) |
| **Performance risk** | None | Low | Medium (IPC bottleneck) |
| **% of real-world files covered** | ~85% | ~95% | ~99% |

---

## 4. Recommended Approach: Two-Phase Strategy

### Phase 1: Browser-native MVP (Option A)

Implement video-file streaming using the `<video>` element + `captureStream()`. This covers the majority of real-world files (MP4/H.264/AAC), ships with zero new dependencies, and reaches users fastest.

**What Phase 1 delivers:**
- File-open dialog filtered to MP4, WebM, and Ogg
- Streaming with synced A/V, seeking, pause/resume, looping
- All existing LiveKit infrastructure works unchanged
- No Rust, no FFmpeg, no build system changes

**Files that won't work in Phase 1:**
- AVI containers (even with H.264 inside)
- MKV containers (even with supported codecs)
- MOV containers (not reliably supported by `<video>`)
- H.265/HEVC video (mostly Linux; sometimes Windows too)
- Any legacy codec (MPEG-2, DivX, etc.)

### Phase 2: Rust-side FFmpeg fallback (Option C)

Add a Rust-side decode path for files the browser can't play. Detect failure in Phase 1 (the `<video>` `error` event), fall back to native decode if available.

**Phase 2 is gated on Phase 1 user feedback** — if real-world usage shows minimal demand for AVI/MKV/HEVC, Phase 2 may never be needed. The Phase 1 implementation should be architected with a source-type discriminator that makes it easy to add a Rust-decoded source type later without refactoring the publish pipeline.

### Why not Phase 0 (WebCodecs + MSTG)?

WebCodecs + MediaStreamTrackGenerator is a middle ground that adds complexity (demuxer, sync clock, seeking) without delivering the format breadth of FFmpeg or the simplicity of `<video>`. It's the worst of both worlds for a first pass. It becomes interesting only if specific WebCodecs features are needed that `<video>` blocks (e.g., frame-level access for overlays), which is out of scope.

### Why not OxideAV?

`oxideav` (pure-Rust video codecs) is promising but was released April 2026 — 3 months old, no v1, incomplete codec coverage. Worth tracking for Phase 2. If it matures, it would eliminate the FFmpeg linking burden entirely and ship as a single Cargo dependency. Subscribe to its releases, but do not couple to it.

---

## 5. Required Architectural Changes (in implementation order)

### Step 1 — Source type abstraction (`packages/shared-types`)

```ts
// packages/shared-types/src/index.ts
export type VideoSourceType = 'screen' | 'video-file';

export interface VideoFileSourceConfig {
  filePath: string;
  fileName: string;
  loop: boolean;
}
```

Extend `StreamSettings` with `sourceType?: VideoSourceType` and `videoFilePath?: string` for persistence across sessions.

### Step 2 — File selection IPC (`apps/desktop/src/main/`)

New IPC handler in `index.ts`:

```
select-video-file: dialog.showOpenDialog() with video file filters
```

New preload bridge method: `selectVideoFile(): Promise<string | null>`

No Rust changes needed.

### Step 3 — Source picker extraction & video-file option (`apps/desktop/src/renderer/`)

Extract the Screenshare Source card from `main.tsx` into a `SourcePicker` component. Add a third option: "Video File" alongside Wayland/X11 sources. On selection, invoke `selectVideoFile` IPC, receive path, display file name.

New state:
- `selectedSourceType: VideoSourceType` (default `'screen'`)
- `selectedVideoFilePath: string | null`
- `selectedVideoFileName: string | null`

Update `canStartShare` to accept `selectedSourceType === 'video-file' && !!selectedVideoFilePath`.

### Step 4 — Video file capture & play (`apps/desktop/src/renderer/`)

New function: `captureVideoTrackFromFile(filePath: string): MediaStreamTrack`

Implementation:
1. Create a `<video>` element (hidden, not in DOM or in an offscreen container)
2. Set `video.src = await getFileUrl(filePath)` — use a custom protocol (`file://` via `protocol.registerFileProtocol`) or a `URL.createObjectURL()` from a `File` object created via `fs.readFileSync`
3. Wait for `loadedmetadata` event
4. Call `video.captureStream()` — returns a `MediaStream` with synced video + audio tracks
5. Return the video track; the audio track is handled alongside

Key detail: `captureStream()` from a `<video>` element includes the audio track automatically. The existing `captureAudioTrack()` (PipeWire capture) is entirely bypassed when `selectedSourceType === 'video-file'`.

### Step 5 — File protocol for local playback (`apps/desktop/src/main/`)

Chromium's sandbox blocks `file://` access from the renderer. Register a custom protocol (e.g., `local-media://`) in the main process that serves file content:

```ts
protocol.handle('local-media', (request) => {
  const filePath = decodeURIComponent(request.url.replace('local-media://', ''));
  return net.fetch(`file://${filePath}`);
});
```

This provides a URL the `<video>` element can play without sandbox violations.

Alternative approach: read the file in the main process via `fs.readFileSync`, create a `Blob` in the renderer via IPC, use `URL.createObjectURL()`. This avoids protocol registration but requires transferring potentially large files through IPC.

### Step 6 — Controls (`apps/desktop/src/renderer/`)

Add seek bar, play/pause toggle, and loop toggle to the stream controls panel. These map directly to `video.currentTime`, `video.pause()`/`video.play()`, and `video.loop`.

New state:
- `videoDuration: number`
- `videoCurrentTime: number`
- `videoIsPlaying: boolean`
- `videoIsLooping: boolean`

### Step 7 — End-of-file handling

When the video reaches the end:
- If `loop === true`: the browser's `<video>` element loops automatically
- If `loop === false`: stop sharing, display "Stream ended" toast, keep room open

A `ended` event listener on the `<video>` element triggers this logic.

### Step 8 — Persistence

Persist `sourceType`, `videoFilePath`, and `loop` to `stream-settings.json` alongside existing `StreamSettings`. Re-hydrate on next launch so the last-used file is remembered.

### Step 9 — (Future) Phase 2: Rust FFmpeg fallback

If Phase 2 is activated:
- Add `ffmpeg-next` crate to `native-rust/Cargo.toml`
- Implement `decode_video_file(path, target_fps)` NAPI export — returns raw frames via callback
- High-bandwidth IPC for frame transfer (shared memory or serialized frame buffers)
- Feed raw frames into `MediaStreamTrackGenerator` writable stream in the renderer
- Audio decode path → feed PCM into `MediaStreamTrackGenerator` audio writable
- Sync clock between audio and video feeds

This is gated and not part of the initial implementation.

---

## 6. Open Risks & Unresolved Questions

### 6.1 Audio routing: bypassing PipeWire

When streaming a video file, the existing `startAudioCapture` / PipeWire path is bypassed entirely. This means:
- No per-app audio metering for the video file (irrelevant — it's not an app)
- Audio is not routed through the physical speakers (video file audio goes directly to WebRTC)
- **The presenter cannot hear the video file's audio locally.** This is a UX problem. Solution: add a `<video>` element that is **not** muted and route audio to the default output device via `audioContext.createMediaElementSource()` → `audioContext.destination`. The `captureStream()` still works regardless of muting.

### 6.2 Non-browser containers (Phase 1 limitation)

MKV, AVI, and MOV files are common. Phase 1 rejects them entirely. Mitigations:
- Clear error message: "This file format is not supported. Convert to MP4 for streaming."
- Recommendation link in the error message
- File filter in the open dialog pre-selects only supported formats
- Phase 2 closes this gap completely

### 6.3 H.265/HEVC unpredictability

Chromium's HEVC support varies wildly by platform. On Windows with HW decoder present, it works. On Linux, it almost never works. Even within H.264, files with uncommon profiles (High 10, High 4:4:4) may fail. The `<video>` `error` event must be caught and surfaced clearly to the user.

### 6.4 Custom protocol security

Registering `local-media://` as a protocol handler opens file:// access from the renderer. This is intentional (the renderer needs to read the video file), but must be scoped tightly:
- Only serve requested files, not directory listings
- Validate that the path was previously selected via the file dialog (use a token or session-scoped whitelist in the protocol handler)
- Reject paths outside the user's home directory (or at minimum, paths the dialog didn't explicitly approve)

### 6.5 Large file handling

Multi-gigabyte video files load slowly and consume significant memory. The `<video>` element streams from disk efficiently (Chromium uses range requests / OS-level file mapping), but startup time for large files can be seconds. Consider a loading state with a progress spinner.

### 6.6 DRM-encrypted content

Files with DRM (FairPlay, Widevine, PlayReady) will not decode in a standard `<video>` element. This is expected behavior — no streaming app should attempt to bypass DRM. The `error` event will fire with `MEDIA_ERR_ENCRYPTED`.

### 6.7 Externally modified files

If the file is moved or deleted after selection but before streaming starts, the custom protocol handler returns a 404-like error. Handle gracefully: toast notification, reset source selection.

---

## 7. Sources Consulted

- [OBS Studio plugins/obs-ffmpeg/obs-ffmpeg-source.c](https://github.com/obsproject/obs-studio/blob/master/plugins/obs-ffmpeg/obs-ffmpeg-source.c) — FFmpeg media source implementation
- [OBS Media Sources documentation](https://obsproject.com/kb/media-sources)
- [Discord Engineering: How It All Goes Live](https://discord.com/blog/how-it-all-goes-live-an-overview-of-discords-streaming-technology)
- [VDO.Ninja](https://vdo.ninja/) — browser-based video file streaming via WebRTC
- [webrtcHacks: All the ways to send a video file over WebRTC](https://webrtchacks.com/all-the-ways-to-send-a-video-file-over-webrtc/)
- [RFC 7742: WebRTC Video Processing and Codec Requirements](https://datatracker.ietf.org/doc/html/rfc7742)
- [W3C WebRTC Extended Use Cases](https://w3c.github.io/webrtc-nv-use-cases/)
- [MDN: HTMLMediaElement.captureStream()](https://developer.mozilla.org/en-US/docs/Web/API/HTMLMediaElement/captureStream)
- [Chrome WebCodecs guide](https://developer.chrome.com/docs/web-platform/best-practices/webcodecs)
- [ffmpeg.wasm GitHub](https://github.com/ffmpegwasm/ffmpeg.wasm)
- [Ideas on Board: "PipeWire is the new v4l2loopback" (Feb 2026)](https://www.ideasonboard.com/news/pipewire-is-the-new-v4l2loopback/)
- [Pion play-from-disk-h264 example](https://github.com/pion/example-webrtc-applications/tree/master/play-from-disk-h264)
- [ffmpeg-next crate on crates.io](https://crates.io/crates/ffmpeg-next)
- [ez-ffmpeg crate on crates.io](https://crates.io/crates/ez-ffmpeg)
- [symphonia crate on crates.io](https://crates.io/crates/symphonia) — pure-Rust audio (no video)
- [oxideav workspace on GitHub](https://github.com/KarpelesLab/oxideav) — pure-Rust media framework (v0, April 2026)
- [gstreamer-rs on GitLab](https://gitlab.freedesktop.org/gstreamer/gstreamer-rs)
- Internal codebase: `apps/desktop/src/renderer/main.tsx` (source picker, capture, publish), `apps/desktop/src/main/index.ts` (IPC handlers), `apps/desktop/src/main/preload.js` (IPC bridge), `packages/shared-types/src/index.ts` (types), `packages/native-rust/src/lib.rs` (NAPI exports)
