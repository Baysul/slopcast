# Native WebRTC Pipeline via LiveKit Rust SDK

**Status:** Research complete  
**Date:** 2026-07-28  
**Decision:** Not yet made — this report provides the architectural analysis needed to decide.

---

## Feature Summary & Assumptions

**Proposed change:** Replace the current Electron/Chromium-based video encoding and WebRTC pipeline with the [LiveKit Rust SDK](https://github.com/livekit/rust-sdks) (`livekit` crate). All screen capture, audio capture, encoding, and WebRTC signaling would move to the Rust native module (`packages/native-rust`). The Electron renderer would become a thin UI shell that issues commands to the native layer and renders previews, but touches no media itself.

**Stated assumptions:**
- The desktop app remains Electron-based (we are not switching to a pure-Rust GUI).
- The signaling server (`apps/server`) is unchanged — it still issues LiveKit tokens.
- The web spectator client is unchanged — it still uses `livekit-client` in the browser.
- The existing PipeWire/WASAPI/CoreAudio audio capture code in the Rust module is retained and reused.
- The combined A/V track idea was also investigated (see separate section below).

---

## Current-State Findings

### What Exists Today

The current pipeline is split across three layers:

| Layer | Video | Audio | WebRTC/LiveKit |
|---|---|---|---|
| **Rust** (`native-rust`) | Nothing — reads PipeWire video node metadata only for audio-source matching | Full capture: PipeWire virtual node creation, port linking, WASAPI loopback, level metering | Nothing |
| **Electron Main Process** | `desktopCapturer.getSources()` for source enumeration; Wayland flags; display media request handler | Routes IPC calls to native module; no audio processing | Nothing |
| **Renderer** (React) | `getDisplayMedia()`/`getUserMedia()` for capture; constraint construction; preview | `getUserMedia({ audio: { deviceId } })` for the Rust-created virtual device; publishes track | `livekit-client` v2.21.0 — Room, signaling, track publishing, codec selection, encoder tuning via `RTCRtpSender.setParameters()`, 1s telemetry polling |

**Key architectural constraints today:**
1. The Rust module has **no video capture, no video encoding, no WebRTC stack** — it does audio capture and PipeWire metadata introspection only.
2. All video codec selection, encoding, and WebRTC signaling goes through Chromium's built-in WebRTC stack in the renderer process.
3. Hardware acceleration is enabled via Chromium CLI flags (`--enable-features=AcceleratedVideoEncoder`, `VaapiIgnoreDriverChecks`, etc.).
4. Audio reaches Chromium via a PipeWire virtual source that appears as a PulseAudio input — the renderer opens it with `getUserMedia({ audio: { deviceId } })`.
5. Simulcast is disabled. Encoder parameters are tuned post-hoc via `RTCRtpSender.setParameters()`.
6. The IPC bridge (`contextBridge.exposeInMainWorld`) carries audio app lists, capture control commands, and stream settings — no video.

---

## Options Considered

### Option A: LiveKit Rust SDK (Recommended)

Replace the entire media pipeline with the `livekit` crate (v0.8.0). This is LiveKit's official Rust client SDK, built on Google's C++ `libwebrtc`.

**How it works:**
1. **Video:** `DesktopCapturer` captures screen/window → produces ARGB frames → CPU-side conversion to I420 → `LocalVideoTrack` → `NativeVideoSource::capture_frame()` → Google libwebrtc encodes (H.264/H.265/VP8/VP9/AV1 with hardware acceleration) → RTP to SFU.
2. **Audio:** Existing PipeWire/WASAPI code captures PCM → feed 10ms chunks (16-bit i16, 48kHz) to `NativeAudioSource::capture_frame()` → Google libwebrtc Opus encoder → RTP to SFU.
3. **Signaling:** `Room::connect(livekitUrl, token)` handles WebSocket signaling, ICE, DTLS — fully native, no browser involvement.

**Pros:**
- Single unified pipeline: capture → encode → publish, all in Rust.
- Direct codec control (no `setParameters()` post-hoc hacks).
- Pre-encoded publish path available (v0.3.42+): can feed already-encoded frames if needed.
- Consistent encoder behavior across platforms (no Chrome version variance).
- Process isolation: media stack runs in a native process, not the Chromium renderer.
- Official LiveKit SDK: first-class protocol support, actively maintained (64 releases, 1.9M downloads).
- Apache-2.0 license, no LGPL entanglements.

**Cons:**
- `libwebrtc` prebuilt binary is large (tens of MB), fetched at build time.
- Still v0.x — breaking changes happen (e.g., `VideoFrame` API change in 0.3.30, `glib-main-loop` becoming opt-in).
- `DesktopCapturer` API relatively young (added v0.3.21, late 2025).
- **No built-in per-application audio capture** — we keep our existing PipeWire/WASAPI/CoreAudio code and feed PCM to `NativeAudioSource`.
- ARGB-to-I420 conversion runs on CPU (the SDK's `yuv_helper` crate).
- Wayland requires `glib-main-loop` feature and `libglib2.0-dev` build dependency.
- First `cargo build` requires network access to download libwebrtc binary.
- No `docs.rs` coverage for most of the crate API (14.77% documented; examples and DeepWiki fill the gap).

### Option B: webrtc-rs + Separate Encoders

Use the pure-Rust `webrtc` crate (webrtc-rs, v0.17.2) with separate encoder crates and reimplement LiveKit's signaling protocol.

**Pros:**
- Pure Rust — no C++ libwebrtc binary, no prebuilt download.
- MIT/Apache-2.0 dual license.
- Mature generic WebRTC stack (5.3M downloads).

**Cons:**
- **No LiveKit integration.** Must reimplement LiveKit's entire signaling layer (protobuf, room management, token auth, track metadata).
- Encoding crates (`rav1e`, `openh264`, `vpx-encode`) are fragmented, variously unmaintained, or single-codec. `rav1e` last updated 407 days ago. `x264` abandoned since Dec 2022.
- The `webrtc` crate is moving to a sans-IO rewrite — the Tokio-based 0.x line is winding down.
- Enormous integration cost — would effectively be building a LiveKit-compatible WebRTC client from scratch.
- **Not recommended.**

### Option C: GStreamer (webrtcbin) Bridge

Use GStreamer's `webrtcbin` element with the LiveKit SFU.

**Pros:**
- Excellent codec support and hardware acceleration.
- Well-understood pipeline model.

**Cons:**
- GStreamer C library is LGPL — creates license complexity for an Electron-distributed app.
- `webrtcbin` has no LiveKit signaling support — same protocol reimplementation problem.
- Adds a heavy system dependency (GStreamer runtime) that must be bundled or installed.
- **Not recommended** — the LiveKit Rust SDK is purpose-built and license-clean.

### Option D: Stay With Current Architecture

Keep Chromium WebRTC in the renderer, Rust audio capture feeding a virtual audio device.

**Pros:**
- Zero migration risk — it works today.
- Chromium's WebRTC stack receives continuous security and performance updates from Google.
- `getDisplayMedia()` portal integration is battle-tested.

**Cons:**
- Two separate media stacks (Chromium for video/WebRTC, Rust for audio capture).
- IPC indirection for audio — Rust creates virtual device, renderer opens it, feeds back to Chromium.
- Encoder tuning is limited to `RTCRtpSender.setParameters()` post-hoc tweaks.
- Codec availability varies by Chromium version and platform.
- H.265 filtered out entirely due to Chromium unreliability.

---

## Combined Audio/Video Track — Investigated and Rejected

**The question:** Since both audio and video capture would be handled by Rust, could we combine both into a single WebRTC track?

**The answer: No. It's not possible and not desirable.**

1. **WebRTC spec forbids it.** `MediaStreamTrack.kind` is strictly `"audio"` or `"video"` — never both. There is no combined track type.
2. **Every major platform uses separate tracks.** LiveKit, Zoom, Google Meet, Discord, OBS/WHIP — all send separate audio and video tracks. This is mandated by the SDP offer/answer architecture (separate `m=audio` and `m=video` sections).
3. **Separate tracks are strictly superior for performance:**
   - Independent codec selection (Opus for audio, VP9/AV1 for video).
   - Independent bandwidth allocation — video can throttle under congestion while audio stays constant.
   - Independent packet loss handling — PLI/NACK for video keyframes, FEC/PLC for audio.
   - SFU fan-out selectivity — bandwidth-constrained clients can skip video layers but always get audio.
4. **Lip sync is already solved.** RTCP Sender Report blocks map RTP timestamps to NTP wall-clock time. Tracks in the same `MediaStream` are synchronized at the receiver using this mechanism. Combining them would not improve sync.
5. **Compatibility would break everywhere.** Combined tracks would be rejected by browsers, LiveKit's track type system (`Track.Source` enum), and the SFU's routing logic.

**Verdict:** Keep separate audio and video tracks, regardless of which pipeline architecture is chosen.

---

## Recommended Approach & Rationale

**Recommendation: Migrate to the LiveKit Rust SDK (Option A), but in phases.**

The LiveKit Rust SDK is the right long-term architecture because:
- It unifies the entire media pipeline in Rust (capture → encode → publish).
- It removes the two-stack split (Chromium WebRTC + Rust audio) and the virtual-device IPC indirection.
- It provides first-class LiveKit protocol support with no reimplementation burden.
- It's the direction LiveKit itself is investing in (the SDK powers their Unity, Python, C++, and Node.js bindings).

**Phased rollout plan:**

| Phase | Scope | Risk |
|---|---|---|
| **Phase 1** | Add `livekit` crate dependency. Implement audio publishing only — feed existing PipeWire/WASAPI PCM to `NativeAudioSource`, connect to LiveKit, publish audio track. Video stays on Chromium. | Low — proves the build, linking, and signaling work end-to-end. |
| **Phase 2** | Add `DesktopCapturer` for video capture on one platform (Linux/X11 first, as it's simplest). Publish video track alongside audio. Keep Chromium path as fallback. | Medium — validates frame conversion, encoding quality, and performance. |
| **Phase 3** | Extend to Wayland, Windows, macOS. Remove Chromium WebRTC path. Clean up `livekit-client` dependency and renderer media code. | Medium — platform-by-platform validation. |
| **Phase 4** | Remove the PipeWire virtual audio device creation entirely. Audio flows direct PCM → `NativeAudioSource`. Clean up `getUserMedia({ audio })` in renderer. | Low — pure cleanup once Phase 2 works. |

---

## Required Architectural Changes (Rough Order)

### Pre-work: Build system
1. Add `livekit` crate to `packages/native-rust/Cargo.toml` dependencies.
2. Configure `webrtc-sys` build — on Wayland, enable `glib-main-loop` feature; ensure `libglib2.0-dev` and other build deps are documented.
3. Handle the first-build libwebrtc binary download in CI and developer setup.

### Phase 1: Native audio publishing
4. Add `Room` management to the Rust module: connect to LiveKit with the token, maintain the connection lifecycle.
5. Expose `native_connect_room(url, token)` and `native_disconnect_room()` napi functions.
6. Feed PipeWire/WASAPI PCM to `NativeAudioSource` and publish as `LocalAudioTrack`.
7. Remove renderer-side `getUserMedia({ audio })` and audio track publishing.

### Phase 2: Native video publishing
8. Implement `DesktopCapturer` source enumeration and capture start/stop.
9. Implement ARGB → I420 conversion (or use the SDK's `yuv_helper`).
10. Publish `LocalVideoTrack` via `NativeVideoSource::capture_frame()`.
11. Wire FPS and resolution controls to the capturer configuration.

### Phase 3: Platform coverage
12. X11: `DesktopCapturer` works directly.
13. Wayland: Enable `glib-main-loop`, test xdg-desktop-portal integration.
14. Windows: Test `DesktopCapturer` with D3D11 hardware encoding.
15. macOS: Test `DesktopCapturer` with VideoToolbox encoding and system picker (`set_sck_system_picker`).

### Post-migration cleanup
16. Remove `livekit-client` from `apps/desktop/package.json`.
17. Remove `getDisplayMedia`/`getUserMedia` renderer code, encoder parameter tuning, codec detection.
18. Simplify IPC bridge — remove audio device enumeration and audio track management channels.
19. Video preview: the Rust layer provides frames that the renderer displays (options: shared memory buffer, canvas rendering, or Electron `OffscreenCanvas`).
20. Telemetry: move `getStats` equivalent to the Rust side (LiveKit SDK provides stats APIs).

---

## Open Risks & Unresolved Questions

1. **libwebrtc binary size.** The prebuilt libwebrtc binary is tens of MB. This will increase the packaged app size. Need to measure exact impact.

2. **Frame conversion cost.** ARGB-to-I420 conversion runs on CPU. For 4K 60fps screen capture, this could be significant. Need benchmarks. NV12 native capture on some platforms (via hardware) may avoid this — investigate `DesktopCapturer` buffer format options.

3. **Video preview mechanism.** Currently the renderer uses `MediaStream` + `<video>` element for local preview. With native capture, the Rust layer must deliver frames back to the renderer for preview. Options:
   - Shared memory buffer + canvas rendering (lowest latency).
   - Electron `desktopCapturer` for preview only while native publishes (hybrid approach during migration).
   - Offscreen rendering via GPU shared texture (complex, platform-specific).

4. **DesktopCapturer maturity.** Added in libwebrtc 0.3.21 (late 2025). As a relatively new API, there may be edge cases with multi-monitor setups, display scaling, window occlusion on Wayland, and HDR surfaces. A spike is warranted before committing.

5. **Build complexity.** `libglib2.0-dev`, `libclang-dev`, `pkg-config`, and other system deps become mandatory. This burdens developer setup and CI. The current PipeWire build already requires some of these, so the incremental cost may be low.

6. **Electron interaction.** The SDK uses a native event loop (optionally glib on Wayland). Need to ensure it coexists with Electron's event loop and Node.js's libuv. The `livekit-runtime` crate abstracts this, but testing for deadlocks is essential.

7. **Token lifecycle.** Currently the renderer fetches a token and connects. If the Rust module handles connections, the renderer must proxy the token to native (straightforward via IPC, but an extra hop).

8. **Wayland source selection UX.** The `DesktopCapturer` API is programmatic — `get_source_list()` → pick a source → `start_capture()`. On Wayland with `xdg-desktop-portal`, the system picker may or may not appear depending on how `DesktopCapturer` invokes the portal. Need to verify the UX matches user expectations.

9. **E2E test impact.** The Playwright e2e test launches Electron and interacts with the renderer. If media handling moves to native, the test must adapt — it can still verify the renderer UI triggers the right behavior and that the spectator sees the stream.

---

## Sources Consulted

- [LiveKit Rust SDK GitHub](https://github.com/livekit/rust-sdks) — README, examples, architecture
- [DeepWiki: LiveKit Rust SDK Video Processing](https://deepwiki.com/livekit/rust-sdks/4.2-video-processing) — encoding pipeline details, codec support, format conversion
- [libwebrtc CHANGELOG](https://github.com/livekit/rust-sdks/blob/main/libwebrtc/CHANGELOG.md) — version history, DesktopCapturer addition, pre-encoded path
- [crates.io: livekit v0.8.0](https://crates.io/crates/livekit) — 1.9M downloads, 64 versions, dependency tree
- [crates.io: libwebrtc v0.3.43](https://crates.io/crates/libwebrtc) — Rust bindings to Google libwebrtc
- [crates.io: webrtc](https://crates.io/crates/webrtc) — webrtc-rs, pure-Rust WebRTC stack (rejected for this use case)
- [screensharing example](https://github.com/livekit/rust-sdks/blob/main/examples/screensharing/src/lib.rs) — DesktopCapturer → ARGB → I420 → publish
- [local_audio example](https://github.com/livekit/rust-sdks/blob/main/examples/local_audio/src/main.rs) — cpal → PCM → NativeAudioSource → publish
- [rust-dev-client reference app](https://github.com/livekit-examples/rust-dev-client) — full wgpu+egui app
- [W3C Screen Capture spec](https://www.w3.org/TR/screen-capture/) — §5.1 track requirements
- [WHIP RFC 9725](https://datatracker.ietf.org/doc/rfc9725/) — OBS-to-SFU standard, separate A/V media sections
- [RFC 8834: Media Transport in WebRTC](https://datatracker.ietf.org/doc/rfc8834/) — RTP/RTCP sync mechanism
- [LiveKit Media Transport docs](https://docs.livekit.io/transport/media/) — track types, screen share setup
- [BlogGeek.me: Lip sync in WebRTC](https://bloggeek.me/lip-synchronization-webrtc/) — NTP/RTP timestamp synchronization
- Crate audits via `crates-audit`: `livekit`, `libwebrtc`, `opus`, `gstreamer`, `ffmpeg-next`, `ffmpeg-sidecar`, `rav1e`, `openh264`, `x264`, `webrtc`
- This project's source: `apps/desktop/src/renderer/hooks/usePresenter.ts`, `apps/desktop/src/main/index.ts`, `apps/desktop/src/main/preload.js`, `packages/native-rust/src/lib.rs`, `packages/native-rust/src/linux/mod.rs`, `apps/server/src/token.ts`, `apps/server/src/routes.ts`, `packages/shared-types/src/index.ts`
