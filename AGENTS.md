# AGENTS.md — Room-Based Native Screensharing & Web Spectator Ecosystem

## 1. Project Intent & Core Architecture

This repository contains a modern, room-based screen and audio sharing system. The ecosystem consists of three primary components:

1. **Desktop Client (Presenter):** Built with **Tauri 2 + React 19 + TypeScript 7 + Vite 6 + Tailwind CSS 4 + shadcn/ui** for the frontend, and a pure-Rust backend (`apps/desktop/src-tauri`) that links the two Rust workspace crates directly — **`native-rust`** (PipeWire/WASAPI capture engine) and **`native-livekit`** (LiveKit room connection + publishing via the `livekit` Rust SDK and its bundled libwebrtc). There is **no Node bridge**: no Electron, no napi-rs, no preload/IPC — the renderer talks to Rust exclusively over Tauri `invoke`/`listen` commands. This is the **only client authorized to capture and broadcast screen and application audio**.
2. **Web Client (Spectator-Only):** A lightweight React browser application allowing external users to join room links/codes instantly. **Web clients are strictly restricted to spectating (receiving WebRTC streams)** and cannot initiate screen or audio capture.
3. **Signaling & SFU Backend Server:** Node.js (Express 5) server exposing a REST API for room creation, code generation (`https://app.domain.com/room/abc-123-xyz`), rate limiting, and LiveKit token issuance. WebRTC signaling itself is delegated to LiveKit, whose Selective Forwarding Unit (SFU) handles audio/video fan-out to support multi-spectator rooms without overloading presenters.

---

## 2. Key System Requirements & Platform Invariants

### A. Room-Based Access & Role Rules
* **Room Generation:** Presenters or users can create rooms on demand. The server generates a unique, cryptographically random room ID or shareable URL slug (format `abc-123-xyz`, validated by `ROOM_CODE_RE` in `@slopcast/shared-types`).
* **Role Enforcement:**
  * **Desktop Application:** Presenter-focused UI. It creates rooms and publishes the screen share + exclusive per-app audio. Spectators join through the web client.
  * **Web Application:** Enforces **Spectator-Only** mode. The Web client UI hides all share controls and contains no capture API calls at all. The hard publish barrier is the SFU: spectator tokens are issued with `canPublish: false` / `canPublishData: false`, so LiveKit rejects any publish from a spectator. As a soft second layer, `POST /api/rooms` requires the `X-Client-Origin: desktop` header the desktop app sends — this is header-based (spoofable) and only keeps honest web clients from minting presenter tokens.
  * **Token minting:** `POST /api/rooms` mints a presenter token (`token`, identity `presenter-<code>-<ts>`, used by the renderer via `connect_native_room`). Spectators get tokens from `GET /api/rooms/:code/token` (identity `spectator-<code>-<ts>`).
  * **Rate limiting:** `express-rate-limit` caps room creation at 10/min and spectator token requests at 30/min.
  * **CORS:** The server's allowlist includes the dev origins (`localhost:3000/5173`) and the packaged Tauri renderer origins (`tauri://localhost`, `http://tauri.localhost`).

### B. Linux / Wayland Window Capture & Exclusive Per-App Audio

**Video capture — libwebrtc `DesktopCapturer` (PipeWire backend) on Linux, WGC on Windows:** the same `livekit::webrtc::desktop_capturer` binding drives both arms; the former in-house portal + `pw_stream` + EGL engine (`native-rust::video_capture`, `portal.rs`/`pw_video.rs`/`egl.rs`) was removed (see SCREEN-CAPTURE-INHOUSE.md, superseded).
* **Capture flow (Linux):** `native-livekit`'s `desktop_capture` module runs the capture thread; the Linux arm is `LinuxDesktopCapture` (`linux_capture.rs`), driving libwebrtc's own PipeWire capturer (`BaseCapturerPipeWire` — `CreateSession` → `SelectSources` → `Start` → `OpenPipeWireRemote` lives upstream in the bundled m144 prebuilt, verified present: `base_capturer_pipewire.o`/`screencast_portal.o`/`shared_screencast_stream.o` in `libwebrtc.a`). `DesktopCaptureSourceType::Generic` → `kAnyScreenContent` (Screen|Window bits), so the KDE picker shows monitors *and* windows — same UX as the old engine. `set_include_cursor(true)` maps to embedded cursor mode (metadata mode is unusable through the binding — the cursor would silently vanish). The portal handshake is async GDBus with **no internal main-context iteration**: `start()` creates the capturer under `MainContext::with_thread_default` (so the proxies bind to our context, never the process-global one the GTK main thread runs) and `poll()` drains that context on the capture thread (`pending()`/`iteration(false)` per tick) — the whole portal flow is single-threaded. The picker opens inside `start_capture`, so the session is reported "running" before the user answers (5-minute picker timeout is portal-side). Frames arrive as **BGRA with a possibly padded stride** — the engine packs rows into the shared tightly-packed contract — and the poll loop drives `capture_frame()` at the encoder target fps. Once the first buffer arrived the capturer re-shares the last frame every poll (static content never stalls, like WGC); `ERROR_PERMANENT` (session closed, captured window gone) fires the `capture-ended` event once. The three KWin corrupt-buffer checks (header-meta/chunk CORRUPTED, MemFd `size==0`) are built into m144's `shared_screencast_stream.cc` — maintained upstream now, not by us.
* **Synthetic source:** `start_synthetic_capture` feeds generated test-pattern BGRA frames through the **exact same conversion and publish path** (no portal picker, no Wayland requirement, available on every platform). It is the default capture route for the headless e2e (`SLOPCAST_E2E_CAPTURE=synthetic`) and the only video route on macOS (which has no capture route at all).
* **Startup invariant:** `native_livekit::arm_pipewire_shims()` must run before any `pipewire-rs` call. libwebrtc's peer-connection factory keeps its PipeWire *video capture module* linked, which drags in hidden-weak `pw_*` dlopen shims; they jump through NULL until `InitializePipewire` arms them (SIGSEGV at startup when unarmed). The Tauri setup arms them, then calls `native_rust::ensure_pipewire_init()`.
* **Platform gating:** Video publish is gated on `platform::video_capture_available` (Wayland on Linux — `platform::is_wayland`: `XDG_SESSION_TYPE=wayland` or a non-empty `WAYLAND_DISPLAY` — or Windows). On X11/macOS `start_native_capture` returns early with `ok: true, videoEnabled: false` and the share degrades to audio-only (the old X11 source picker / `desktopCapturer` / `getDisplayMedia` paths are gone entirely); the renderer shows a `PlatformNotice` gate screen there.
* **Windows desktop capture — libwebrtc `DesktopCapturer` (WGC):** the capture engine is `WgcCapture` (`native-livekit/src/wgc_capture.rs`, a `CaptureSource::Wgc` arm of `run_capture`), driving libwebrtc's `DesktopCapturer` with the WGC backend through the livekit crate's **bundled bindings** (`livekit::webrtc::desktop_capturer` — libwebrtc 0.3.44 over webrtc-sys 0.3.41, already in the dependency tree; no forks, no patches). The C++ shim already wires `allow_wgc_screen_capturer`/`allow_wgc_window_capturer` (with DXGI as the pre-Win10-2004 fallback and `_WIN64` only), disables enumerating our own windows, and embeds the cursor on `include_cursor`. Frames arrive as **BGRA with a possibly padded stride** — the engine packs rows into the shared tightly-packed contract — and the capture loop polls `capture_frame()` at the encoder target fps (WGC re-delivers the last frame on static content, so the stream never stalls). The capture thread is **COM-initialized (MTA)**; `ERROR_PERMANENT` (captured window closed) fires the existing `capture-ended` event once, mirroring the portal `session_closed` semantics. **Windows has no system picker**: the renderer's `CaptureSourcePicker` lists screens/windows from the `get_capture_sources` command and the selection rides into `start_native_capture`/`start_capture_preview`/`go_live` as an optional `source` param (ignored on Linux, required on Windows). Requires Windows 10 2004+ for WGC itself; minimized windows freeze at their last frame and DRM-protected content is blacked out (both matching Chromium).
* **Single-layer publish:** video is published as a single layer at the configured quality (no simulcast/SVC). The capture engine scales frames to the encoder target `(width, height, fps)` from the stream settings (`ScaleTarget`) before pushing them, so a 4K source is encoded at the user's chosen resolution, not 4K.
* **Keepalive delivery (portal path):** KWin's screencast stream is event-driven — a frame is recorded only when the compositor renders (damage) or the cursor moves (`scheduleRecord` coalesces events into one frame per tick), so a low-activity screen delivers far fewer frames than the refresh rate. When the delivery pacer finds its queue empty on a tick, the loop re-pushes the last delivered frame (same immutable I420 buffer, refreshed timestamp) so the RTP cadence stays even instead of stalling; the encoder compresses the duplicates to near-nothing and the receiver never sees gaps. The WGC engine already behaves this way (its capturer re-delivers the last frame on static content).
* **Codecs:** The native stack can encode `h264`/`vp8`/`vp9`/`av1` (`get_native_supported_codecs`; `h264` is the only codec with a hardware encoder factory in the bundled libwebrtc — VA-API/NVENC on Linux, Media Foundation on Windows, VideoToolbox on macOS). The renderer's codec picker reads this list and **never** the webview's `RTCRtpSender.getCapabilities` (the WebKitGTK/GStreamer stack is not used for encoding). Stream settings default to `vp8`: the bundled libwebrtc's VA-API H264 path has proven unreliable on Linux (encoder collapses to ~1-3 fps shortly after publish); H264 remains selectable for hardware-encode setups where it works. **Known bitstream bug (2026-08-09):** the VAAPI H264 encoder shim in vendored `webrtc-sys 0.3.41` keeps POC state in function-local `static`s (`calc_poc`) that a *dropped keyframe* leaves stale (`key_frame_request` is cleared before the `kEmptyFrame` skip) — the next P-frame then gets a +256 POC discontinuity that only **hardware** decoders mis-handle (software decode is clean; the bug also corrupts multi-stream/reuse/threaded use). Also hardcodes `level_idc = 41` (Level 4.1), under-declaring 1080p60 (needs 4.2) and 1440p+ streams. See `VAAPI-H264-BITSTREAM-BUG.md` for the full mechanism and fix plan.
* **Preview transport:** Preview frames travel BGRA over a **raw Tauri channel** (`register_preview_channel`, `InvokeResponseBody::Raw`) at the stream framerate (30 fps fallback, 60 fps cap). The payload is self-describing: a 16-byte little-endian header (`u64 pts_us`, `u32 width`, `u32 height`) followed by tightly packed BGRA rows — no base64, no JSON per frame. **Scaling is SIMD libyuv, off the capture thread's critical path:** the capture callback stashes the newest frame's **I420 planes** (a single mutex-guarded slot, ~2.25 MB for 1080p — a third of the old raw-BGRA copy) *after* the encoder conversion, and the preview emitter copies the planes out under a brief lock and does `I420Buffer::scale` + `i420_to_argb` (fitted to the renderer's card via `set_preview_viewport`, OBS-style "scale to the window", `GetScaleAndCenterPos` fit math, aspect preserved, never upscaled) **outside** it — neither side ever holds the shared lock for a scale or convert, so a slow emission cannot stall the capture callback (the original scalar bilinear ran under the lock: a ~40 ms scale at card size throttled the whole capture to ~20 fps). **libyuv naming trap:** the wrappers are FOURCC-rotated — `i420_to_argb` writes `[B, G, R, A]` (the renderer's `bgra8unorm` order); the similarly named `i420_to_bgra` writes `[A, R, G, B]`. The renderer (`PreviewCanvas`) uploads the BGRA bytes into a **persistent GPU texture** per frame and draws it: WebGPU (`bgra8unorm` + `queue.writeTexture`) when `navigator.gpu` exists, otherwise **WebGL2** (raw BGRA uploaded labeled RGBA, channels swapped in the fragment shader at sample time — zero-copy; WebKitGTK's WebGL2 rejects `TEXTURE_SWIZZLE` and lacks `EXT_texture_format_BGRA8888`), with a Canvas2D `putImageData` fallback. **WebGPU is not available in the shipped Linux webview**: WebKitGTK builds with `ENABLE_WEBGPU=OFF` (WebKit's CMake default) and exposes no runtime toggle — `navigator.gpu` is undefined there, so the WebGL2 path is the one that runs. **Pre-roll flow:** `start_capture_preview` (capture-only, frames flow to the preview, nothing published) → `go_live` (publishes the track against the running capture, or starts capture + publish combined). `bench_register_channel` / `bench_push_frames` benchmark raw channel throughput (see `tests/e2e/bench-preview.spec.ts`).
* **Capture stats:** `get_video_capture_stats` returns `framesDequeued` / `framesPushed` / `framesDropped` / `captureErrors` / `previewFramesSent` / `lastWidth` / `lastHeight` (reset per capture start). The e2e requires `framesPushed` and `videoFramesEncoded` to advance while streaming.
* **Auto-end when the captured window closes:** the compositor stops the screencast stream and the portal closes the ScreenCast session (`Session::Closed` — libwebrtc's `BaseCapturerPipeWire` watches the same signal and flips `capturer_failed_`). The engine's next `capture_frame()` poll reports `ERROR_PERMANENT`, which fires the one-shot capture-ended callback (both arms share `fire_capture_ended_once`), forwarded as a `capture-ended` Tauri event; the renderer tears the share down through the Stop path (track + capture + audio, stage → idle) with a "Stream ended — the captured window was closed" toast. Monitor/region sources never close on their own, so the event fires only when the captured source is actually gone; our own `stop()` never triggers it (the engine is dropped, closing the session, after the poll loop exits).
* **Wayland capture-source introspection (`get_capture_context`/`inspect_capture_context`):** video-node inspection runs natively in Rust (no `pw-dump` subprocess) and reports the desktop environment (KDE/GNOME), the source type (monitor/window/region), and the best-matched audio app for the captured source. KWin names its screencast streams `kwin-screencast-<desktopFileName>` for windows and `kwin-screencast-<outputName>` for monitors (`ScreencastManager::streamWindow`/`streamOutput`) — never the window UUID. The suffix is classified as window/monitor/region; a window suffix is resolved through KWin's scripting D-Bus interface via `zbus` (`src/linux/kwin.rs`) — the same mechanism `kdotool` uses, without the external binary. The one-shot script matches the window by `desktopFileName` (fallback: `resourceClass`, for X11/XWayland clients) and reports its PID and caption; matching then tries, in order: exact PID (native apps, Wine/Proton), the window process's `/proc` names (browsers, whose audio utility PID differs from the window PID), the window caption, and finally the desktop file name itself. The backend snapshots the highest `object.serial` of `kwin-screencast-*` nodes **before** triggering the portal and only accepts a node created *after* that point, so lingering portal-preview streams are never mistaken for the live capture. The results are cached in `CaptureContextCache` (managed state) and carry `portal.screencast.*` metadata, `window_pid` and `window_caption`.

**Exclusive Per-Application Audio in Rust (`pipewire-rs`):** modelled on the OBS Studio plugin [obs-pipewire-audio-capture](https://github.com/dimtpap/obs-pipewire-audio-capture):
* The Rust native module connects to PipeWire and creates a dedicated virtual capture node (`Slopcast-Window-Audio`) via the `adapter` factory with `factory.name=support.null-audio-sink`, with a channel layout mirroring the system default sink (stereo fallback).
* The node's `media.class` MUST be `Audio/Source/Virtual`, never `Audio/Sink`. Chromium's PulseAudio backend discards every source that is a sink monitor (`AudioManagerPulse::InputDevicesInfoCallback`), so a null sink's `.monitor` can never appear in the renderer's `enumerateDevices()`. A virtual source is a first-class PulseAudio source and still exposes input ports to link into.
* It links **only** the selected target application's `Stream/Output/Audio` ports into the capture node's input ports. The capture target is matched primarily by PipeWire **client.id** (the application's connection to the PipeWire daemon), falling back to process ID and binary name. This guarantees that every audio stream from the target application is captured, regardless of PID metadata quirks (e.g., Discord's `WEBRTC VoiceEngine` stream often lacks a process ID in its properties). **No other application's audio is ever linked in.**
* **Crucial Rule:** The target application's existing links are never touched, so its audio MUST continue playing through the user's physical speakers/headphones, unaffected.
* **Target semantics (`AudioTarget`):** `Id` carries the numeric target — a PipeWire node id on Linux, a process ID on Windows; `-1` selects system audio ("Desktop Audio" mode, which deliberately links *every* application's streams in); Linux values **below** `-1` are process-id targets (`-pid`, for apps with no active audio stream yet — e.g. a paused player — whose audio is linked the moment it starts playing). `Label` carries a textual target resolved through the nine-strategy cascade `find_best_audio_match` (exact name, cleaned name, containment, acronym, `/proc` cmdline, significant word, first word, window title) and the Wayland captured-window resolution (`resolve_audio_app_for_captured_window`).
* **Delivery path:** The capture node's PCM is delivered straight to the native publisher — native-rust `set_audio_data_callback` → src-tauri `register_audio_callbacks` → `feed_pcm` → native-livekit `NativeAudioSource` (48 kHz stereo i16, bounded drop-newest PCM channel). The renderer never creates or publishes a JS audio track.
* All PipeWire operations must use native C bindings in `pipewire-rs`. Do not spawn shell CLI tools (`pw-link`, `pactl`). The node is created with `object.linger=false` so it dies with the worker's PipeWire connection instead of leaking like a `pactl`-loaded module.
* **`dump_audio_sources`:** prints every live audio stream node's full property dictionary (registry props merged with bound-node info props — the same view `pw-dump` prints) as a debug aid for auto-resolve misses; the renderer logs it fire-and-forget at capture start.

**Per-app waveform metering (`start_audio_metering`):** a dedicated worker thread taps every `Stream/Output/Audio` node with a `pw_stream` (AUTOCONNECT, additive links only — existing speaker links are never touched) and reports per-node waveform envelopes. The process callback only downmixes each quantum to mono and pushes it into a lock-free `ArrayQueue` (drop-newest when full); a throttled wave pass (~33 ms cadence) on the same worker thread drains the ring into a rolling 4096-sample window (~85 ms at 48 kHz, ~60% overlap between passes) and decimates it into 96 interleaved (min, max) amplitude pairs — raw per-column extrema, no smoothing needed. A meter whose ring has received no samples for 150 ms is treated as paused: the pass publishes zeros instead of re-decimating its stale window, so bars flatline instead of staying pinned. The pairs are published under a mutex and read non-destructively, so push-cadence jitter in the host process cannot distort values (pushed at 33 ms). Delivery: `set_wave_callback` in native-rust → src-tauri `audio.rs` dedups with the shared `WAVE_EPSILON` (0.002, defined in `@slopcast/shared-types` so the two epsilons can never drift apart) and emits an `audio-wave-update` Tauri event. The renderer canvas draws the peak of each pair as a rounded bar symmetric around a center baseline (Apple Voice Memos / iMessage voice-note style; 96 columns decimate 2:1 into ~48 bars), with the bar height mapped through a −48 dBFS floor so sources below 100% volume stay readable. Each bar additionally runs through a per-bar envelope follower (leaky integrator: instant attack so the 33 ms data cadence is never smeared, exponential ~3.5/s release) — the classic SoundCloud/AES peak-falloff technique — so bars fall smoothly instead of hopping between updates. Window titles come from node **info** props (`media.name`), not registry props — nodes must be bound before their full props are visible.
* **CSS identifier limitation for window titles:** Chromium-family browsers expose no tab/window titles. Chromium hardcodes every playback stream's `media.name` to `"Playback"` and routes all tab audio through one audio-service PID, so its per-tab streams are indistinguishable in PipeWire (Firefox sets real tab titles via `media.name`; native apps usually do too).
* **MPRIS enrichment for audio stream titles:** On Linux, the native layer queries the session D-Bus for MPRIS players (`org.mpris.MediaPlayer2.*`) and matches them to audio applications by PID (fallback: normalized identity/desktop-entry containment). The `xesam::title` metadata is exposed as `mediaTitle` on each audio app. This provides meaningful now-playing titles for Chromium-family apps (via `plasma-browser-integration` or the browser's own MPRIS player) and media players like Spotify, whose PipeWire streams otherwise carry only a generic app name.
* **Player row consolidation in the picker:** The renderer collapses all streams belonging to the same PipeWire client (or app name when client id is unavailable) into a single picker row (`utils/audio-grouping.ts`). This aligns the Linux UI with the cross-platform reality — Windows (WASAPI process loopback) captures at the application level, not per-stream. The row subtitle shows the MPRIS now-playing title, falling back to the first member's PipeWire window title, then "N audio streams" when more than one stream is grouped. Process IDs are never displayed.

### C. Tauri Backend & Renderer Integration

* The Rust backend replaces the Electron main process: every former preload IPC channel maps to a `#[tauri::command]` in `apps/desktop/src-tauri/src/` (`audio.rs`, `capture.rs`, `context.rs`, `platform.rs`, `room.rs`, `settings.rs`, `config.rs`). The renderer calls them **only** through the typed wrapper `apps/desktop/src/renderer/api/desktop.ts` (`desktopApi` over Tauri `invoke`/`listen`). Every call degrades gracefully: a missing/rejected command resolves to a typed fallback (`false`/`null`/defaults) with a one-time warning, so the renderer never crashes on a rejected invoke.
* **Command surface:** `get_app_config`, `get_platform_info`, `probe_gpu_info`, `get_audio_apps`, `dump_audio_sources`, `start_audio_capture`/`stop_audio_capture`/`switch_audio_capture`, `start_audio_metering`/`stop_audio_metering`, `resolve_audio_source`, `get_capture_context`/`inspect_capture_context`, `get_stream_settings`/`save_stream_settings`, `get_onboarding_completed`/`set_onboarding_completed`, `connect_native_room`/`disconnect_native_room`/`is_native_room_connected`, `get_spectator_count`, `get_native_telemetry`, `get_native_supported_codecs`, `start_native_capture`/`start_capture_preview`/`go_live` (all three take an optional Windows WGC `source` selection — ignored on Linux, required on Windows), `get_capture_sources` (Windows-only WGC screen/window enumeration), `start_synthetic_capture`, `update_native_video` (re-publish encoder settings without restarting capture), `stop_native_capture` (track + capture + audio), `stop_video_capture`, `is_native_capture_active`, `get_video_capture_stats`, `register_preview_channel`, `set_preview_viewport`/`clear_preview_viewport`, `bench_register_channel`, `bench_push_frames`. Events: `audio-wave-update`, `capture-ended` (portal session closed or WGC `ERROR_PERMANENT` — the captured window/app was closed, the renderer auto-stops the share); preview frames arrive on a raw `Channel<InvokeResponseBody>`.
* **Window chrome:** the native titlebar is disabled (`decorations: false` in `tauri.conf.json`) — `TitleBar` (`components/layout/TitleBar.tsx`) owns the drag region (`data-tauri-drag-region` on the bar and branding block only, so the control buttons keep clicks) plus minimize/maximize/close over `windowControls` (`api/windowControls.ts` → `getCurrentWindow()`, frontend window APIs, not backend commands — they live outside `desktopApi` and degrade gracefully like it). The capability file grants `core:window:allow-{close,is-maximized,minimize,start-dragging,toggle-maximize}`. The titlebar renders on **every** screen (including the X11 gate) so the undecorated window is never undraggable; the app shell is `h-screen` with a `TitleBar` and a single `overflow-y-auto` content area. Room controls (Create Live Room, share code, copy actions) live in the Screenshare Source card (`SourcePicker`), not a header row. Double-click maximize comes from the OS native drag path. **KDE Wayland caveat:** tao 0.35.x ignores `decorations: false` on KDE Wayland (tao#899 — KWin forces server-side decorations; fixed in tao 0.36.0/#1218, which tauri 2.11 can't resolve since it pins `tao = "0.35"`). The workspace root `Cargo.toml` carries a `[patch.crates-io]` pin of tao to the fix commit — **remove the patch once a tauri release depends on tao ≥ 0.36** (wry dev already uses 0.36, so tauri 2.12 should).
* **Window icon on Wayland:** the compositor resolves a window's icon by matching its `app_id` against an installed `<app_id>.desktop` file — there is no window-icon protocol on Wayland, so `set_icon` is a no-op there. The app_id is the GTK program name = the binary name (`slopcast`, verified live: KWin logs `appid = "slopcast"`); KWin's lookup is an exact, case-sensitive `QStandardPaths` match, so the desktop file **must** be named `slopcast.desktop`. The bundled `.deb` ships that file (productName is lowercase `slopcast` so tauri-bundler's `{productName}.desktop` naming matches; the display name stays `Slopcast` via the custom deb template `src-tauri/templates/main.desktop`, wired in `bundle.linux.deb.desktopTemplate`). Dev builds install no desktop entry, so KDE shows the generic Wayland icon — run `pnpm desktop:install-entry` once per machine (installs `apps/desktop/resources/slopcast.desktop` + the icon into `~/.local/share/{applications,icons/hicolor/512x512/apps}`); the icon is resolved at window map time, so restart the app afterwards. Note: AppImage/NSIS bundle filenames follow the lowercase productName too (`slopcast_0.1.0_amd64.AppImage`, `slopcast_0.1.0_x64-setup.exe`).
* **Room flow:** the renderer POSTs `/api/rooms` (with `X-Client-Origin: desktop`), then `connect_native_room(url, token)`; the room's `livekitUrl` from the response wins over config. `native-livekit` runs a dedicated worker thread with its own tokio runtime: it publishes the audio track (NativeAudioSource) on connect, and video tracks on demand via `start_video_track`. Spectator count and connection state are polled (1 s cadence) — native-livekit does not push events; an unexpected drop after a seen-connected state triggers a "Room disconnected" toast.
* **Telemetry:** `get_native_telemetry` folds libwebrtc `RtcStats` on the worker thread (outbound-rtp bytes/packets/frames, codec mime, `encoderImplementation`, frame width/height, remote-inbound RTT/loss, epoch timestamp), with a 500 ms answer timeout. The renderer computes deltas/smoothing exactly like the old `RTCRtpSender.getStats()` path. **Units trap (fixed 2026-08):** libwebrtc stats timestamps are **microseconds** since the epoch; the DTO field is `timestampMs` and the Rust side converts (`timestamp / 1000`) — without the conversion every renderer rate (fps, video/audio bitrate) was 1000× too small (a healthy 28 Mbps stream read as 28 kbps, 60 fps read as 0.06 → displayed "0 fps"). The e2e asserts the rendered telemetry-bar DOM (`[data-testid="telemetry-fps"]`), not just the invoke path.
* **Plugins & security:** plugins for single-instance, window-state, clipboard-manager, opener and log (log level Info — the plugin's default Trace forwards every libwebrtc `debug!` from ICE threads, flooding the console with STUN spam). Permissions are capability-granted (`src-tauri/capabilities/default.json`: `core:default`, clipboard write-text, window-state, opener, log). A strict CSP lives in `tauri.conf.json` (`connect-src` allows `ipc:`/`http://ipc.localhost`, the Vite dev server, `http://localhost:3001` and `wss:`/`https:`). The renderer is a webview with **no Node access** — all privileged work happens in Rust commands behind the `desktopApi` wrapper; never bypass it or add a command without a renderer-side typed fallback.
* **e2e-only cargo feature `e2e`:** embeds `tauri-plugin-wdio` (`browser.tauri.execute`) + `tauri-plugin-wdio-webdriver` (WebDriver HTTP server on `TAURI_WEBDRIVER_PORT`). Production builds exclude them — the WebDriver server is an unauthenticated localhost HTTP surface. The renderer only loads `@wdio/tauri-plugin` when `VITE_E2E=1`.
* **App config:** `slopcast.config.json` is parsed by **both** the server (`packages/shared-types/src/config.ts` `loadConfig`) and the Tauri backend (`config.rs`, a serde mirror): env vars > file > defaults, upward directory search from cwd, production assert that rejects `devkey`/`secret` unless `ALLOW_DEV_KEYS=true`. The renderer receives only `apiEndpoint` + `livekitUrl` via `get_app_config`.
* **Stream settings persistence:** `StreamSettings` = `fps` (default 60), `bitrateLimit` (20 Mbps), `videoCodec` (`vp8`), `resolution` (preset: `480p`/`720p`/`1080p`/`1440p`/`2160p`, default `1080p`), `apiEndpoint` (`http://localhost:3001`). Persisted as `stream-settings.json` (plus `onboarding.json`) in the app config dir (`~/.config/slopcast` on Linux) via the `settings.rs` commands. **TS↔Rust sync rule:** `DEFAULT_STREAM_SETTINGS`/`sanitizeStreamSettings` in shared-types mirror `default_stream_settings`/`sanitize_stream_settings` in `settings.rs` field-for-field (same defaults, clamps and whitelists); the `defaults_match_ts_table` conformance test enforces the values — update both files together. Renderer writes are debounced (`SETTINGS_SAVE_DEBOUNCE_MS`, 800 ms) so a burst produces one save and one "Stream settings saved" toast; hydrated saved settings take precedence over config-file defaults; the codec picker derives from `get_native_supported_codecs` and falls back to `vp8`.
* **Lifecycle:** on `ExitRequested` the backend stops audio capture, audio metering, the video track and desktop capture (mirrors Electron's `before-quit`).

---

## 3. Model Context Protocol (MCP) Tooling Configuration

AI Agents operating within this codebase MUST leverage the following MCP tools:

| MCP Server | Domain / Target | Agent Use Case |
| :--- | :--- | :--- |
| `shadcn-ui-mcp` | UI Design System | Retrieve, scaffold, and adapt shadcn/ui components for both Desktop and Web frontends to ensure uniform aesthetics. |
| `rust-analyzer-mcp` | Native Backend — the whole Rust workspace: `/packages/native-rust`, `/packages/native-livekit`, `/apps/desktop/src-tauri`, `/probe`, `/packages/native-rust/xtask` | Type-checking, AST inspection, `cargo check`, and dependency management for the `pipewire-rs`, WASAPI and `livekit` modules. |
---

## 4. Repository Directory Layout

```text
.
├── apps/
│   ├── desktop/               # Tauri 2 Desktop Presenter
│   │   ├── src/renderer/      # React + TS + Tailwind 4 + shadcn/ui (Presenter UI)
│   │   │   ├── api/desktop.ts     # Typed desktopApi wrapper over Tauri invoke/listen
│   │   │   ├── components/        # audio/, gate/, layout/, onboarding/, preview/,
│   │   │   │                      # settings/, sources/, telemetry/, ui/ (shadcn)
│   │   │   ├── hooks/             # useNativeRoom, useAudioCapture, useStreamSettings,
│   │   │   │                      # useStreamTelemetry, useOnboarding, useNotificationSound
│   │   │   └── utils/             # audio-grouping, audio-level-store, codecs, clipboard
│   │   ├── src-tauri/         # Rust backend (slopcast crate): lib.rs, audio.rs, capture.rs,
│   │   │                      # config.rs, context.rs, dto.rs, platform.rs, room.rs, settings.rs,
│   │   │                      # e2e.rs (wdio plugins, feature-gated), capabilities/, tauri.conf.json
│   │   ├── tests/             # renderer unit tests (node --test) + e2e specs (wdio)
│   │   ├── resources/         # App icon (README)
│   │   ├── wdio.conf.ts       # WebdriverIO config (embedded WebDriver, tauri-service)
│   │   └── vite.config.ts, tsconfig.renderer.json
│   ├── web/                   # Web Application (React + TS + shadcn/ui - Spectator Only)
│   │   ├── Dockerfile, docker-entrypoint.sh, public/config.js
│   └── server/                # Room Management & Signaling Server (REST + LiveKit)
│       ├── Dockerfile, slopkit-livekit.yaml.example
│       └── src/
│           ├── index.ts           # Express app entrypoint (CORS, rate limits, route mounting)
│           ├── routes.ts          # Room creation, spectator token issuance, health
│           ├── roomCodes.ts       # Cryptographic room code generation (node:crypto)
│           ├── token.ts           # LiveKit presenter/spectator token grants
│           └── e2e-test.ts        # Playwright + WebdriverIO end-to-end harness
├── packages/
│   ├── native-rust/           # Rust capture engine (pipewire-rs, zbus, WASAPI/windows)
│   │   ├── src/
│   │   │   ├── lib.rs             # Public API: audio capture, metering, matching cascade
│   │   │   ├── audio_ring.rs      # PCM ring buffer + telemetry
│   │   │   ├── linux/             # apps.rs, capture.rs, graph.rs, kwin.rs,
│   │   │   │                      # metering.rs, mpris.rs, procinfo.rs,
│   │   │   │                      # screencast.rs (introspection)
│   │   │   └── windows/           # WASAPI process-loopback capture & filtering
│   │   └── xtask/                 # cargo xtask check-targets (multi-platform type check)
│   ├── native-livekit/        # Rust LiveKit publisher (livekit crate + bundled libwebrtc)
│   │   └── src/
│   │       ├── lib.rs             # Room worker, tracks, telemetry, codec list
│   │       ├── desktop_capture.rs # Shared capture pipeline: pacer, preview, stats
│   │       ├── linux_capture.rs   # Linux PipeWire capturer (libwebrtc DesktopCapturer)
│   │       └── wgc_capture.rs     # Windows WGC capturer (libwebrtc DesktopCapturer)
│   └── shared-types/          # Shared TypeScript interfaces + config loader + StreamSettings
│       └── src/ (index.ts, config.ts)
├── probe/                     # Capture probes (connect_probe, probe_a/b, probe-capture.sh)
├── Cargo.toml                 # Rust workspace (native-rust, native-livekit, src-tauri, probe)
├── package.json               # pnpm workspace root scripts
├── pnpm-workspace.yaml
├── biome.json                 # Single root Biome config (formatter + linter)
├── docker-compose.yml         # livekit + slopcast-server + slopcast-web + nginx (Caddy optional)
├── nginx.conf, Caddyfile      # Reverse-proxy entry points
├── .github/workflows/         # build.yml (push CI) + release.yml (tagged releases)
├── slopcast.config.json.example
└── .cargo/config.toml, .nvmrc
```

---

## 5. Code Quality & Linting

**Prioritize readability, maintainability, and minimalism. Every line of code is a liability. When in doubt, do less.**

### TypeScript / JavaScript / JSON (Biome)

Follow these rules:
- No `any`, no unnecessary type casts or non-null assertions.
- No speculative abstractions — don't build for hypothetical future use cases.
- Prefer early returns over nested conditionals. Keep functions small and single-purpose.
- Comments explain *why*, not *what*. Remove comments that just restate the code, and remove dead/commented-out code.
- Don't extract shared code until it's duplicated 3+ times.
- No silent error handling — no empty catches, no defensive try/catch for impossible cases.
- Don't add libraries, config options, params, or files that aren't needed for the stated goal.
- Match the existing formatting/lint conventions and don't touch unrelated code.
- Prefer the simplest solution that solves the actual problem, not the most general one.
- Ternary operators: only use a ternary for a trivial, single-line assignment with exactly two simple, side-effect-free branches, e.g.:
const label = isActive ? 'Active' : 'Inactive';

Never do any of the following — use if/else, a switch, or a small named function instead:
- Nested or chained ternaries (a ? b : c ? d : e)
- Ternaries that span multiple lines or need extra parentheses to read
- Ternaries whose branches call functions, mutate state, throw, or have any side effect
- Ternaries used for control flow rather than producing a value
- Ternaries where either branch is itself a complex expression (object literal, JSX with multiple elements, etc.)

This project uses [Biome](https://biomejs.dev/) (2.5.5) as the unified formatter and linter for all JavaScript, TypeScript, JSX, TSX, and JSON files. EditorConfig, Prettier, and ESLint are replaced entirely by Biome.

**Configuration:** Root-level `biome.json` with VCS-aware ignores (respects `.gitignore`), formatter (120-char line width, 2-space indentation, single quotes, semicolons, trailing commas), linter (`recommended` ruleset plus `noExplicitAny`, `noNonNullAssertion`, `noNestedTernary`, `noExcessiveCognitiveComplexity`, `noUnusedVariables`/`noUnusedImports`, ...), import organizer, and Tailwind CSS directive parsing. The `apps/server/src/e2e-test.ts` override allows `noExplicitAny` for Playwright locator chains — do not remove it without verifying the test still passes Biome CI.

### Rust (rustfmt + clippy)

Follow these rules:
- No reflexive `.clone()` to dodge the borrow checker — fix the underlying ownership/borrowing instead.
- No `.unwrap()`/`.expect()` on fallible paths in production code — propagate errors with `?` and proper error types.
- Don't introduce generics, traits, or `dyn` dispatch for a single implementation — only generalize on a real second use case.
- Prefer early returns and `?` over deeply nested `match`/`if let`. Keep functions small and single-purpose.
- Comments explain *why*, not *what*. Every `unsafe` block needs a `// SAFETY:` comment. Remove dead/commented-out code.
- Don't extract shared code until it's duplicated 3+ times. Keep `pub` surface minimal — prefer `pub(crate)`.
- Don't add crates, config, or files that aren't needed for the stated goal.
- Code must be `rustfmt`- and `clippy`-clean, with no blanket lint suppressions (targeted `#[allow(..., reason = "...")]` is fine).

## Style Rules

**Spacing**

Group a function body into up to four blocks, in this order, each separated
from the next by exactly one blank line. Within a block, no blank lines.
Omit any block that doesn't apply — don't force empty groups.

1. **Guards** — early returns / validation, back-to-back, no blank lines
   between them.
2. **Definitions** — `let` bindings that compute or gather values used below.
3. **Logic** — the calls and mutations that do the actual work.
4. **Return** — the final expression or `Ok(...)`, alone.

```rust
fn process_order(order: &Order, inventory: &Inventory) -> Result<Receipt, OrderError> {
    if order.items.is_empty() {
        return Err(OrderError::EmptyOrder);
    }
    if !inventory.has_stock(&order.items) {
        return Err(OrderError::OutOfStock);
    }

    let total = order.items.iter().map(|i| i.price * i.qty).sum();
    let discount = compute_discount(order);
    let final_total = total - discount;

    inventory.reserve(&order.items)?;
    let receipt = Receipt::new(order.id, final_total);

    Ok(receipt)
}
```

Notes on the example: the two guards have no blank line between them (same
block). The three `let`s computing the total are one block. The reserve call
and receipt construction are "logic" because they do work, not just compute a
value, so they get their own block. `Ok(receipt)` stands alone.

**Naming**
- Standard Rust casing: `snake_case` for functions/variables/modules,
  `UpperCamelCase` for types/traits/enums, `SCREAMING_SNAKE_CASE` for
  consts/statics.
- Names should reveal intent. Avoid generic catch-alls (`data`, `info`, `val`,
  `tmp`, `thing`) — name for what the value represents, not its type or role.
- Booleans get `is_` / `has_` / `should_` / `can_` prefixes.
- Functions get verb or verb-phrase names describing what they do
  (`parse_config`, not `config_stuff` or `handle_config`).
- Avoid implementation-shaped type suffixes (`-Impl`, `-Helper`, `-Manager`)
  unless the suffix is doing real descriptive work.
- Use the same name for the same concept everywhere — don't call it `id` in one
  module and `identifier` in another.
- Single-letter names only in tight, obvious scopes (loop indices, short
  closures like `|x| x + 1`) — not for anything spanning more than a few lines.
- Abbreviations only where idiomatic in Rust/the domain (`ctx`, `cfg`, `idx`);
  don't invent new ones.

**Expression complexity**
- Keep `match`, `if`, and `while` scrutinees simple: a variable, a field
  access, or one short method call. If producing the value takes more than
  one chained call, bind it to a named local first.
- Don't nest a multi-line closure inside another expression — a match
  scrutinee, a chained call, a function argument. Name it: either a local
  binding or, if it's reused, a function. This is usually why `rustfmt`
  output looks awkward — it's formatting an inherently tangled expression,
  not making a bad choice.

All Rust code must conform to the [Rust Style Guide](https://doc.rust-lang.org/stable/style-guide/) and pass `cargo clippy --all-targets -- -D warnings`. The `[lints.clippy]` config in **all three Rust crates** (`native-rust`, `native-livekit`, `apps/desktop/src-tauri`) enables `pedantic` plus hard `deny` on `unwrap_used`, `expect_used`, `undocumented_unsafe_blocks`, and `allow_attributes_without_reason`.

**Configuration:** `packages/native-rust/rustfmt.toml` and `packages/native-livekit/rustfmt.toml` apply 2024 edition defaults with 100-char line width, import reordering, and module reordering; `apps/desktop/src-tauri` formats with rustfmt defaults.

**Key Commands:**

| Command             | Description                                                          |
| ------------------- | -------------------------------------------------------------------- |
| `pnpm check`        | Biome CI + `pnpm --recursive check` (per-package gates, see below)   |
| `pnpm check:fix`    | Apply all safe Biome auto-fixes                                      |
| `pnpm lint`         | Run Biome linter only (read-only)                                    |
| `pnpm lint:fix`     | Run Biome linter + apply safe fixes                                  |
| `pnpm format`       | Biome format + `pnpm --filter @slopcast/native-rust rust:fmt`        |
| `pnpm rust:fmt`     | Run `cargo fmt` on `@slopcast/native-rust`                           |
| `pnpm rust:check`   | Run `cargo fmt --check` on `@slopcast/native-rust` (CI mode)         |

**Per-package check gates** (what `pnpm --recursive check` runs):
- `desktop`: `tsc -p tsconfig.renderer.json --noEmit` (renderer type-check gate) + `cargo fmt --check` + `cargo clippy --all-targets -- -D warnings` in `src-tauri`
- `@slopcast/native-rust`: `cargo fmt --check` + `cargo clippy --all-targets -- -D warnings` + `cargo xtask check-targets`
- `@slopcast/native-livekit`: `cargo fmt --check` + `cargo clippy --all-targets -- -D warnings`
- `server` / `web` / `@slopcast/shared-types`: `tsc`/`tsc --noEmit`

**Agent Rules:**
1. Agents MUST run `pnpm check` after making any code changes and fix all failures before declaring a task complete.
2. All build and package scripts (`build`, `build:desktop`, `package:desktop`, `dist:*`) run `pnpm check` as a prerequisite gate; a build is rejected if `biome ci`, `cargo fmt --check`, or `cargo clippy -- -D warnings` fails.
3. Use the `style` commit type for Biome/rustfmt/clippy-related changes (whitespace, formatting, lint fixes with no logic change).
4. The `biome.json` config must never be overridden per-package. All workspace members are covered by the single root configuration.
5. The `rustfmt.toml` config must never deviate from the Rust Style Guide defaults. Do not add `unstable_features = true` or any configuration that requires nightly.
6. Clippy is enforced by the `check` scripts (`cargo clippy --all-targets -- -D warnings`, inheriting the `[lints.clippy]` config in each Rust crate). Prefer a real fix (renaming, restructure, `// SAFETY:` comment, targeted `#[allow(..., reason = "...")]`) over broad suppressions; every `unsafe` block needs a `// SAFETY:` comment.

**Multi-platform Rust type check:** `packages/native-rust` runs `cargo xtask check-targets` (a lightweight Rust binary at `xtask/`) which type-checks the `#[cfg(target_os)]` platform modules the host build cannot see (`cargo check --target …`), so shared-struct/API drift in the Windows module (e.g. E0063) fails locally instead of in CI. Linux hosts check the Windows target (`x86_64-pc-windows-msvc`); Windows hosts check only their own module. The linux target is never cross-checked because `pipewire-sys`/`x11` bind against Linux system headers at build time.

---

## 6. Development Guidelines for AI Coding Agents

### Frontend & UI Guidelines (Desktop + Web)
1. **Shared Visual Design:** Both `apps/desktop` and `apps/web` use shadcn/ui (cva + Radix primitives, `components.json` present in both) and Tailwind CSS 4. Use `shadcn-ui-mcp` to generate components.
2. **Web Browser Guardrails:** In `apps/web`, ensure `navigator.mediaDevices.getDisplayMedia` is explicitly blocked or omitted. Render a prominent UI banner informing users: *"Web spectators can view screenshares. To host or share your screen, please open the Desktop App."* (`SpectatorBanner`). The web client upgrades server-advertised `ws://` LiveKit URLs to `wss://` on HTTPS pages (`normalizeLivekitUrl` in shared-types).
3. **Presenter Controls (Desktop):** Provide a clear Window Audio Source picker panel (`AudioAppPicker`) listing active audio applications (queried from the Rust native layer). The presenter selects exactly one target application; ONLY that application's audio is captured for the stream.
4. **Audio Source Auto-Refresh (Desktop):** The renderer re-polls `get_audio_apps` every 3 s (`AUDIO_APPS_POLL_MS`). `list_audio_applications()` is a read-only PipeWire enumeration on a throwaway connection — it must never touch `CAPTURE_STATE`/`METER_STATE`, and the picker selection (keyed by PipeWire node id) is never mutated by a refresh, so auto-refresh cannot interrupt an active stream.
5. **Stream Settings Persistence (Desktop):** Stream settings (fps, bitrate limit, video codec, resolution preset, API endpoint) persist to `stream-settings.json` in the app config dir (`~/.config/slopcast` on Linux) via the `get_stream_settings`/`save_stream_settings` Tauri commands; the shape and defaults (`StreamSettings`/`DEFAULT_STREAM_SETTINGS`) live in `@slopcast/shared-types` and are mirrored field-for-field in Rust (`settings.rs`) — update both together. Renderer writes are debounced (`SETTINGS_SAVE_DEBOUNCE_MS`, 800 ms) so a burst of changes produces one save and one "Stream settings saved" toast; both the Rust and TS sanitizers validate per-field so hand-edited or corrupt files can never crash the app. The video codec defaults to `vp8` (see §2B).
6. **Renderer ↔ Backend Integration (Desktop):** All backend access goes through `api/desktop.ts` (`desktopApi`) — never call `invoke` directly in components. Preview frames arrive on a raw `Channel` (16-byte LE header `pts_us`/`width`/`height` + tightly packed BGRA rows) and are drawn by `ScreensharePreview`/`PreviewCanvas` (WebGL2 texture upload, WebGPU when available).

### Tauri Security Model
The renderer is a sandboxed webview with `nodeIntegration`-equivalent access **disabled by construction** (no Node in the webview): all privileged work runs in Rust commands, plugin permissions are capability-granted (`src-tauri/capabilities/default.json`), and a strict CSP is set in `tauri.conf.json`. Never bypass the `desktopApi` wrapper, never expose a command that accepts free-form paths or shell input, and never weaken the CSP. The `e2e` cargo feature must stay out of production builds (unauthenticated localhost WebDriver surface).

### Build & Package Commands

| Command | Description |
|---|---|
| `pnpm dev:desktop` | Run the desktop app in dev mode (`tauri dev`, renderer on :5173) |
| `pnpm dev:web` / `pnpm dev:server` | Web spectator / API server dev servers |
| `pnpm build:desktop` | `pnpm check` + `pnpm --filter desktop tauri build --no-bundle` (binary only) |
| `pnpm package:desktop` | `pnpm check` + `pnpm --filter desktop tauri build` (AppImage + deb + nsis) |
| `pnpm dist:desktop` | Build + produce all configured bundles (appimage, deb, nsis) |
| `pnpm dist:desktop:linux` | Build Linux AppImage + deb (`--bundles deb,appimage`) |
| `pnpm dist:desktop:linux:appimage` | Build Linux AppImage only |
| `pnpm dist:desktop:linux:deb` | Build Linux deb only |
| `pnpm dist:desktop:linux:tar` | Build deb, then tarball its payload to `target/release/bundle/Slopcast-<version>-linux-amd64.tar.gz` |
| `pnpm dist:desktop:win` | Build Windows NSIS installer |
| `pnpm test:unit` | Server + shared-types + desktop renderer unit tests |
| `pnpm test:e2e` | Full end-to-end harness (see §7) |

**Artifacts land in `target/release/bundle/{appimage,deb,nsis}/`** (Cargo workspace target dir at the repo root). The `e2e` test binary is built separately with `VITE_E2E=1 pnpm --filter desktop tauri build --features e2e` (add `--no-bundle` when AppImage bundling is unavailable in the environment).

### Deployment & CI
* `docker-compose.yml` runs the full stack: `livekit` (`--dev --bind 0.0.0.0 --node-ip ${LIVEKIT_NODE_IP:-10.0.0.128}` — `--node-ip` makes the SFU advertise a host-routable ICE candidate instead of the container bridge IP; ports 7880 tcp / 7881 tcp / 7882 udp), `slopcast-server` (healthcheck on `/health`), `slopcast-web`, and `nginx` as the public entry point (port 80, proxying `/` → web, `/api/*` + `/health` + `/ws` → server; a commented-out Caddy alternative provides automatic TLS).
* **Two LiveKit URLs:** `LIVEKIT_URL` is how the API server reaches the SFU (`ws://livekit:7880` inside the compose network — only the server can resolve it); `LIVEKIT_CLIENT_URL` is what gets advertised to browsers and the desktop app in token responses and must be reachable from clients (`ws://localhost:7880` for same-machine testing, `wss://…` for remote spectators). Never advertise the container hostname to clients.
* Production LiveKit: drop `--dev`, set `LIVEKIT_KEYS`, expose the UDP media range (50000–60000 outside dev mode) plus TCP 7881, and set `--node-ip`/`rtc.use_external_ip` so the SFU advertises a reachable address.
* **GitHub Actions:** `build.yml` (push to `main`/`refactor`) builds server + web Docker images (linux/amd64 + arm64 → ghcr.io) and the desktop matrix (appimage/deb/tar/win) via the `dist:*` scripts. `release.yml` (tags `v*`) additionally pushes provenance attestations and creates a GitHub release. Known inconsistency: the workflow `asset-path` globs (`apps/desktop/release/*`) still point at the Electron-era output location while Tauri builds emit to `target/release/bundle/` — reconcile when next touching CI.
* Config precedence (server and desktop alike): env vars > `slopcast.config.json` (upward search) > defaults; production refuses `devkey`/`secret` unless `ALLOW_DEV_KEYS=true`.

---

## 7. Testing & Verification Workflows

### Unit Tests
* Server: `pnpm --filter server test` — `node --test` via tsx over `src/*.test.ts` (`roomCodes`, `routes`, `token`).
* `@slopcast/shared-types`: `index.test.ts` (room-code regex, settings sanitizer, URL normalization).
* Desktop renderer: `pnpm --filter desktop test` — `node --test` over `tests/*.test.ts` (`audio-grouping`, `audio-level-store`, `codecs`).
* Rust: unit tests in `native-rust` (matching cascade, settings-conformance mirror of TS), `native-livekit` (`yuv_order_probe`), and `src-tauri` (`settings.rs` `defaults_match_ts_table`, `platform.rs` GPU probe marked `#[ignore]` for manual runs).
* `pnpm test:unit` runs all of the above.

### Automated End-to-End Test (`pnpm test:e2e`)

The harness lives at `apps/server/src/e2e-test.ts` and orchestrates **two automation phases**: a **WebdriverIO presenter phase** driving the real Tauri binary (embedded WebDriver via the `e2e` cargo feature, spec at `apps/desktop/tests/e2e/presenter.spec.ts`) and a **Playwright Chromium spectator phase**. It runs one full presenter → spectator pass **per codec** (`E2E_CODECS`, default `h264,vp8,vp9,av1`).

**Prerequisites:**
```bash
pnpm install
pnpm exec playwright install chromium          # spectator browser
VITE_E2E=1 pnpm --filter desktop tauri build --features e2e   # presenter binary (--no-bundle ok)
# livekit-server must be on PATH (or LIVEKIT_URL must point at a reachable SFU)
pnpm test:e2e
```

**What it validates:**

| Step | Description |
| :--- | :--- |
| **Config & setup** | Parses `slopcast.config.json`, kills conflicting port processes, spawns API server + Web dev server with health polling (30 s timeout). Optionally detects and launches Spotify. |
| **LiveKit preflight** | TCP-checks the configured `livekitUrl`. For localhost endpoints the harness always runs its own `livekit-server --dev` (a listener on the port is NOT trusted — containerized SFUs often relay signaling but fail ICE/DTLS on the media plane); otherwise fails fast with an actionable error. Also kills stray app instances (`pkill -f target/release/slopcast`) — `tauri-plugin-single-instance` would silently hijack the launch. |
| **Presenter (WebdriverIO + Tauri)** | Runs the WDIO spec against the `--features e2e` binary with an isolated app config dir (`XDG_CONFIG_HOME` → `test-output/e2e-userdata`) so real persisted settings cannot leak in. The spec: asserts the Wayland gate (portal mode only), clicks "Create Live Room" and extracts the room code from `span.font-mono`, starts the screenshare → preview canvas appears → Go Live → `[role="status"]` LIVE badge. In synthetic mode (`SLOPCAST_E2E_CAPTURE=synthetic`, the default) the backend feeds a test pattern through the real publish path — no portal picker, no Wayland session needed; `stream-settings.json` (720p@30, 8 Mbps, the pass codec) is pre-written for each pass. Telemetry + capture stats are sampled ~2 s apart: `videoFramesEncoded`/`videoBytesSent` must advance, the reported outbound codec must match the pass codec, `previewFramesSent > 0` (raw-BGRA preview emitter ran), and a published-fps floor is enforced. GPU diagnostics come from `probe_gpu_info` (dlopen'd EGL probe replacing `app.getGPUInfo`): `softwareRasterizer` must be false and `eglVendor` present. Progress is written to `presenter-phase.json`; the harness polls it and only hands off to the spectator once `handoffReady` flips, then writes a release flag so the spec's hold test (which keeps the app alive during the spectator phase) ends gracefully. |
| **Spectator (Chromium)** | Headless Chromium via Playwright navigates to the room URL. Asserts the connection badge (`[role="status"]`), a `<video>` element with non-zero dimensions that is actively playing, and — in synthetic mode — that the received stream is **exactly the published 1280x720 single layer** (a halved simulcast layer fails the run). |
| **Video capture malfunction checks** | Presenter-side: native telemetry must advance AND `framesPushed > 0` (catches black-keepalive publishing: track up, capture dead). Spectator-side: two consecutive `requestVideoFrameCallback` frames (~1 s apart) must arrive (continuous flow, not a single-frame stall) and the decoded pixels must not be uniformly black (64x64 canvas luma/variation check). A `[data-decoder-stalled]` UI flag (codec/profile mismatch) is treated as an error. All recorded in `e2e-result.json`. |
| **Presenter-stop propagation** | Mid-hold the harness requests a stop (`E2E_STOP_FLAG` → spec clicks Stop Screenshare → confirm, acks `E2E_STOPPED_FLAG`); the spectator's `[role="status"]` badge must leave "Live" and report the stream ended **while the presenter stays connected** (its room-lifetime audio track keeps the publication alive — the web spectator keys stream liveness off video publications, so a stale "Live" badge fails the run). |
| **Diagnostic validation** | Parses all console logs for fatal patterns (SEGV, uncaught exceptions, ICE failure, GPU process crash, decoder stall, `framesDecoded=0`). GPU feature status from the probe report — fails if software-rendered. Writes structured JSON result with pass/fail and per-codec outcomes. |
| **Retry & cleanup** | `try/finally` guarantees the spectator browser, the WDIO session (release flag → graceful teardown → SIGTERM), and spawned servers are shut down and ports reaped even when a step throws; console logs and the result JSON are written on every outcome. The whole run retries up to 2 attempts with a root-cause summary after all attempts are exhausted, and exits non-zero if every attempt fails. |

**Output artifacts** (written to `test-output/`):

| File | Content |
| :--- | :--- |
| `desktop-console.log` | All captured console entries from every source (server, web, wdio/tauri backend logs, livekit, spectator) |
| `web-console.log` | All DevTools console output from the Chromium spectator |
| `desktop-gpu-report.json` | Raw `probe_gpu_info` output — EGL vendor, GL renderer/version, software-rasterizer flag |
| `presenter-phase.json` | Structured presenter-phase result (room code, telemetry, GPU report, errors) written by the WDIO spec |
| `e2e-result.json` | Structured `TestResult`: room code, video dimensions, per-codec results, GPU status, errors, duration, retries |
| `e2e-userdata/` | Isolated app config dir (stream-settings.json per pass, onboarding state) |

**Agent rules for the e2e test:**
1. The test scripts (`apps/server/src/e2e-test.ts`, `apps/desktop/tests/e2e/presenter.spec.ts`) must pass `pnpm check` before being considered complete.
2. The `biome.json` override for `e2e-test.ts` allows `noExplicitAny` for Playwright locator chains. Do not remove this override without verifying the test still passes Biome CI.
3. When adding new UI elements to the desktop or web app, update the WebdriverIO selectors in `presenter.spec.ts` and the Playwright assertions in `e2e-test.ts` to match (`span.font-mono` room code, `[role="status"]` badges, `[data-decoder-stalled]`, preview canvas).
4. Do not commit `test-output/` or `.opencode/` directories.

### Manual Walkthrough: Room Creation & Web Spectators
0. One-time dev setup: run `pnpm desktop:install-entry` so the Wayland taskbar shows the real app icon for dev builds (see §2C).
1. Start LiveKit in dev mode: `docker run --rm -p 7880:7880 -p 7881:7881 -p 7882:7882/udp livekit/livekit-server --dev --bind 0.0.0.0` (or `docker compose up -d livekit`).
2. Start the server: `pnpm dev:server` — Web client: `pnpm dev:web` — Desktop app: `pnpm dev:desktop`.
3. Click "Create Room" in Desktop app -> copy generated link (`http://localhost:3000/room/XYZ123`).
4. Open the link in a browser (Chrome/Firefox) -> verify Web client auto-connects as Spectator.
5. Start screenshare in Desktop app -> verify high-fps video and exclusive window audio stream in Web browser.

### Testing PipeWire Exclusive Window Audio on Linux
Execute the following verification commands:
```bash
# 1. Verify active PipeWire nodes and process names
pw-dump | grep -E "application.name|node.name"

# 2. Confirm virtual capture node creation upon screenshare start
pw-cli list-objects Node | grep "Slopcast-Window-Audio"

# 3. Verify audio links: ONLY the target application's ports may be linked
#    into the capture node (and it must also stay linked to the physical sink)
pw-link -l

# 4. Confirm the capture node is exposed as a real source, not a sink monitor
#    ("Monitor of Sink: n/a"), otherwise Chromium will never enumerate it
pactl list sources | grep -A 6 "Name: Slopcast-Window-Audio"
```

### Testing the Packaged Desktop Build
```bash
pnpm dist:desktop
# Bundles land in target/release/bundle/
# Linux: appimage/slopcast_0.1.0_amd64.AppImage, deb/slopcast_0.1.0_amd64.deb,
#        Slopcast-0.1.0-linux-amd64.tar.gz (via dist:desktop:linux:tar)
# Windows: nsis/slopcast_0.1.0_x64-setup.exe
```

---

## 8. Key Dependency Versions

| Package | Version |
|---|---|
| Tauri (Rust backend + frontend API) | ^2.11 (@tauri-apps/api ^2.11.1, @tauri-apps/cli ^2.11.4) |
| React | ^19.2.0 |
| TypeScript | ^7.0.0 |
| Vite | ^6.0.0 |
| Tailwind CSS | ^4.0.0 (via @tailwindcss/vite) |
| Biome | 2.5.5 |
| livekit (Rust SDK, bundled libwebrtc) | 0.8 |
| livekit-server-sdk (Node) | ^2.17.0 |
| livekit-client (web) | ^2.21.0 |
| Express / express-rate-limit | 5.2.1 / ^8.6.1 |
| pipewire (Rust crate) | 0.10.0 |
| zbus | 5 |
| windows / windows-core (WASAPI) | 0.62.2 |
| WebdriverIO + @wdio/tauri-service | ^9.30.1 / ^1.3.0 |
| Playwright | ^1.55.0 |
| Node.js (required) | >= 24.0.0 (pnpm 9.15.4, `.nvmrc`) |

---

## 9. Git Commit Conventions

All commits must follow the [Conventional Commits](https://www.conventionalcommits.org/) format with a strict type prefix.

### Commit Format
```
type: short description (max 72 chars)

Optional body with additional context. Blank line between
subject and body is required when body is present.
```

### Allowed Types
| Type     | When to Use                                               |
|----------|-----------------------------------------------------------|
| `feat`   | A new feature or user-facing capability                   |
| `fix`    | A bug fix                                                 |
| `refactor` | Code change that is neither a fix nor a feature         |
| `chore`  | Build, tooling, dependency updates, or CI changes         |
| `docs`   | Documentation-only changes (README, AGENTS.md, etc.)      |
| `test`   | Adding or updating tests                                  |
| `style`  | Formatting, whitespace, lint fixes (no logic change)      |
| `perf`   | Performance improvement                                   |

### Rules
1. **First line is the commit message.** Keep it under 72 characters. Use lowercase after the type prefix. Do not end with a period.
2. **Body is optional.** Include it when the subject alone is insufficient. Wrap at 72 characters.
3. **One logical change per commit.** Avoid combining unrelated changes.
4. **Stage only intended files.** Run `git status` and `git diff` before committing to verify.
5. **Never commit secrets, API keys, or large generated assets.**
6. **Do not commit `.opencode/` or other tool-specific config directories** unless explicitly asked.

### Examples
```
feat: add room code generation with nanoid

fix: resolve PipeWire audio PIDs via /proc fallback

refactor: extract AudioAppPicker into shared component

chore: bump Tauri to v2.11

docs: add git commit conventions to AGENTS.md
```
