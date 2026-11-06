# Desktop migration to GPUI Kit

Proposed plan, 2026-11-06. Repository inspected at `e09bebc`. The plan sections below are unchanged; see [Implementation status](#implementation-status) for what now exists and what has been verified. Being written down here does not establish that GPUI works with Slopcast's media stack on real capture hardware.

Replace the desktop React/Tauri/CEF application with a Rust application using GPUI Kit. Preserve the existing native media engines and server protocol. Build the new desktop executable alongside the current one, prove native rendering and desktop integration first, and switch distribution only after feature and packaging parity.

Assumed scope: Linux Wayland remains the primary target; preserve Linux amd64/arm64 and Windows x64 distribution. Preserve the current X11 capability notice/audio behavior rather than adding X11 screen capture. macOS support, a server rewrite, a browser-client rewrite, and media-engine redesign are separate projects.

## What the repository already provides

| Area | Current implementation | Migration treatment |
| --- | --- | --- |
| Video capture, encoding, publication | `packages/native-livekit`; Linux GStreamer/PipeWire, Windows capture and LiveKit paths | Retain, including `publisher_session.rs`, `frame_delivery.rs`, and the `vendor/webrtc-sys` patch. |
| Application audio and metering | `packages/native-rust`; Linux PipeWire and Windows audio code | Retain. Preserve PCM delivery, mute, source resolution, waveform deduplication and application grouping. |
| Native command layer | `apps/desktop/src-tauri/src/{audio,capture,room,context,settings,preferences,config}.rs` | Extract application services and models from Tauri handles, state injection, events and async helpers. |
| Presenter behavior | Renderer `main.tsx`, `hooks/useNativeRoom.ts`, `useAudioCapture.ts`, `useStreamSettings.ts`, telemetry hooks and utilities | Port behavior into Rust services and GPUI views. This includes substantial application logic, not only layout. |
| Preview transport | `capture.rs` callback/cache → `frame.localhost` → `utils/preview-frames.ts` → `PreviewCanvas.tsx` | Replace HTTP/IPC polling and browser canvases with an in-process frame handoff and GPUI image rendering. |
| Desktop integration | `controls.rs`, `shortcuts.rs`, `tray_pulse.rs`, `window_drag.rs`, `window_icons.rs`, `diagnostics.rs` | Port lifecycle behavior; replace Tauri-specific implementations. Keep useful portal/notification code. |
| Settings | JSON persistence through `SettingsStorage`, including selectable settings directory | Reuse schema, validation and atomic writes; preserve existing locations. |
| Tests | Rust media tests, TypeScript logic tests, desktop Playwright/CDP harness | Retain media tests; port desktop logic tests; replace desktop browser driving with GPUI tests/native integration. |
| Distribution | Tauri manifests, bundle scripts, CEF caches and build/release workflows | Replace desktop packaging while retaining GStreamer preparation and required native libraries. |
| Spectators/API | `apps/web`, `apps/server`, shared TypeScript packages | Keep working throughout. React and pnpm remain needed for these applications. |

The existing media crates have no Tauri dependency in their manifests. This is the main reason to migrate the shell and application coordination without rewriting capture/publication.

## Framework choice and dependency policy

Use the `gpui-kit` application facade with its styled components and assets, importing GPUI through the facade. Its current source provides initialization, window creation and a feature-gated UI test harness. Do not independently select incompatible GPUI/component versions. [Facade source](https://github.com/longbridge/gpui-kit/blob/b2aaa3e5f9a387dea74981586bfca33e7e08420d/crates/kit/src/lib.rs), [getting started](https://github.com/longbridge/gpui-kit/blob/b2aaa3e5f9a387dea74981586bfca33e7e08420d/website/docs/getting-started.md).

Research snapshot: upstream `b2aaa3e5f9a387dea74981586bfca33e7e08420d`. The inspected workspace declares Kit `0.9.0` and pins `gpui-pre` packages to `0.3.11`, while its README still shows a `0.7` dependency example. Treat that as documentation/version drift, not evidence of a verified published release. Phase 1 must resolve and build an exact released version or an exact Git revision, commit the lockfile, and record the selected source. [Workspace manifest](https://github.com/longbridge/gpui-kit/blob/b2aaa3e5f9a387dea74981586bfca33e7e08420d/Cargo.toml), [README](https://github.com/longbridge/gpui-kit/blob/b2aaa3e5f9a387dea74981586bfca33e7e08420d/README.md).

Start with the default GPUI backend. Do not enable optional GPUI Fast, JavaScript extensions or a webview during the migration. Native Rust views are the target; retaining the existing React renderer inside another browser would leave the principal migration unfinished.

Linux GPU compatibility is a gate: upstream's installation guide requires a graphical Wayland/X11 session and working Vulkan driver. Derive our minimal runtime dependencies from the selected build rather than copying the gallery's installation list, which includes optional-feature dependencies. Verify compatibility with the repository's pinned Rust toolchain and Windows MSVC configuration. [Installation requirements](https://github.com/longbridge/gpui-kit/blob/b2aaa3e5f9a387dea74981586bfca33e7e08420d/website/docs/installation.md).

## Intended architecture

Add `packages/desktop-core` and `apps/desktop-gpui` as Cargo workspace members. Keep `apps/desktop/src-tauri` as the compatibility frontend during migration. Use separate executable/package names until cutover; do not rename the existing tree at the same time as extracting behavior.

- `desktop-core`: presenter state, room API client, capture coordination, settings, endpoint validation, audio selection/grouping, telemetry transformations, commands and events. It depends on the existing media crates, but neither GPUI nor Tauri.
- `desktop-gpui`: GPUI bootstrap, views, theme/assets, UI model, window lifecycle and platform adapters for tray, shortcuts, notifications, clipboard, dialogs, external links and instance activation.
- Existing Tauri app: thin adapters into extracted services, retaining its current command/event contract while the new shell reaches parity.

The core owns the presenter session for the lifetime of the process. Main and settings windows subscribe to it; hiding or closing a window must not own or terminate the room heartbeat. UI buttons, tray items and shortcuts dispatch the same typed commands.

Represent room and capture state separately: room connection can be reconnecting while capture is still active, and a room can remain open after sharing stops. Model pending operations explicitly; serialize conflicting start/stop/switch/close requests. Tag async completions with operation/session generations so late portal, HTTP and source-selection results cannot revive a stopped session. If an obsolete request creates a room or capture resource, release it rather than merely ignoring the result.

Give the core an explicitly owned Tokio runtime for HTTP, timers and portal work replacing Tauri's async helpers. Keep GPUI updates on its UI executor and blocking capture/device/file work off that thread. Preserve existing media-owned threads, GLib contexts and the non-Linux LiveKit worker runtime initially; do not consolidate these runtimes during shell migration. Cancellation and shutdown must not wait on the UI thread for work that needs that thread to finish.

Use bounded command/event delivery with explicit overflow handling. Coalesce meter/telemetry snapshots and keep only the newest preview frame; preserve terminal events such as capture-ended and room-closed. Register process-global native callbacks once and unregister them on shutdown. The existing native singleton APIs mean the two shells are alternative executables, not two independent presenters embedded in one process.

## Implementation sequence

**1. Prove the native shell and media integration.**

Create a minimal `desktop-gpui` executable with the pinned Kit dependency, main/settings windows and an isolated development settings directory. Link both existing native crates in the same executable. First render synthetic preview frames, then exercise real Wayland capture and Windows capture, audio and publication to the existing browser spectator.

Exercise the risky desktop operations in this spike: keep the process alive with all windows hidden, reopen from the tray/second launch, invoke a Wayland portal shortcut, display a modal confirmation, and quit cleanly. Select the tray implementation only after proving event-loop compatibility; do not assume a tray library that needs a GTK loop can simply run inside GPUI. A Linux StatusNotifier/D-Bus adapter and a Windows native tray adapter are viable approaches to investigate.

Measure the existing application first on the same machines and workload: startup, process-tree memory, idle/preview/live CPU and GPU usage, preview latency, encoded FPS/drops and bundle size. Repeat with the spike, separating UI cost from media cost. Set acceptance budgets from these observations; this plan promises no numerical performance improvement.

**Exit:** Linux Wayland and Windows native smoke runs succeed, Linux arm64 builds, media and GPU dependencies coexist, preview resources remain bounded, tray/shortcut feasibility is demonstrated, and a dependency/runtime decision is recorded. Stop the broad UI port if native rendering, event-loop integration or required packaging is blocked.

**2. Extract a reusable Rust application core.**

Move DTOs, configuration loading, settings sanitization/storage and native media coordination into `desktop-core`. Inject paths and service dependencies instead of passing `AppHandle`. Replace `tauri::async_runtime` calls in extracted code with the owned runtime and replace string events with typed core events. Leave Tauri command names and payloads stable through wrappers.

Preserve the settings files `stream-settings.json`, `onboarding.json`, `desktop-preferences.json` and the default-directory marker `settings-location.json`. Match the actual old Tauri config path on each OS; do not assume another directory library uses the same convention. Honor relocated directories and use the existing atomic-write behavior. Test old files in place, missing files, malformed values, unavailable custom directories and rollback reads. Avoid schema changes: preferences currently reject unknown fields.

Preserve config discovery and environment overrides from `config.rs`. Keep resource lookup and logging paths behind shell-provided adapters so both development and installed binaries work.

**Exit:** the current desktop still works through the extracted core; the core has no Tauri/CEF/GPUI dependencies; existing Rust behavior tests and targeted adapter/serialization tests pass.

**3. Move presenter orchestration out of React.**

Port the workflow from `main.tsx` and `useNativeRoom.ts` into the core, then have the existing renderer consume core snapshots and commands. Remove each old timer/owner when its replacement is connected: do not run two heartbeat or reconnect coordinators.

Preserve these behaviors explicitly:

- Room creation, preparation/discard, share link/code, close-key headers, spectator count, deletion and failed-delete confirmation.
- Presenter heartbeat every 20 seconds with a 10-second timeout, token refresh and the distinction between room closure, transient error and an older server without the heartbeat endpoint. Port `presenter-heartbeat.test.ts` fixtures.
- Native session reconnect status and delayed notices without building a second media reconnect engine alongside `publisher_session.rs`.
- Endpoint health checks, confirmed endpoint switching, replacement-room cleanup/retry, and prevention of stale results from the previous endpoint.
- Preview before publication, go-live without unnecessarily reopening the picker, source switching, portal cancellation/capture-ended cleanup, stop-sharing versus close-room, and explicit quit/quit-anyway behavior.
- Automatic audio matching, manual selection, mute, codec availability/fallback, bitrate recommendations, saved setting validation and telemetry history/smoothing.

Use typed Rust HTTP models and fixture tests against the existing server contract; retain TypeScript shared types for the web/server. Send the existing desktop-origin and close-key headers. Keep token/close-key ownership in memory and out of diagnostics. Preserve endpoint validation and TLS behavior when replacing browser fetch.

**Exit:** state-machine tests cover racing start/stop/switch/close operations, stale completions, refresh, reconnect and failure cleanup; a synthetic presenter successfully streams to the unchanged browser client through the core.

**4. Implement the GPUI presenter and settings views.**

Use Kit buttons, inputs, selectors, sliders, switches, dialogs and notifications where appropriate. Build small custom elements for preview, audio meters/waveforms and telemetry. Translate theme tokens and typography preferences from the existing CSS; bundle font formats supported by the selected GPUI text backend with their licenses. Preserve keyboard navigation, focus restoration, disabled/busy states and accessibility labels.

Port in working slices: room creation/link sharing; preview and go-live; source/audio controls; telemetry; stream settings; general/appearance/typography/shortcut settings; onboarding and platform notices. Have both windows observe the same model rather than recreating browser cross-window events. Keep input entities and subscriptions alive across renders.

For preview, initially adapt the existing callback payload: a 16-byte little-endian timestamp/width/height header followed by packed BGRA pixels. Validate dimensions and length, keep an owned latest frame, and use the selected GPUI image API with explicit channel order/stride handling. Preserve aspect ratio and register viewport dimensions in physical pixels. Clear the viewport when hidden and restore it when visible so publication continues without needless preview conversion.

Do not route frames through JSON, HTTP, PNG/JPEG encoding or an unbounded event queue. Verify image/texture eviction across sustained frame updates and resize; allocating a fresh cached image forever is not acceptable. Start with a measured CPU-buffer upload path. GPU texture import/zero-copy is a later optimization only if measurements justify it.

**Exit:** the ordinary create-room → pick-source → preview → live → switch/mute → stop → close flow works entirely in GPUI, both settings windows stay synchronized, preview memory stabilizes during sustained use, and keyboard/UI tests cover the core flow.

**5. Complete desktop service parity.**

Replace Tauri plugins with explicit adapters; reuse GPUI's platform facilities where the pinned version supports the required operation. Reuse `rfd` and `notify-rust` where compatible rather than reimplementing those services unnecessarily.

- Preserve tray status/pulse, menu enabled states, show-main/settings actions, close-to-tray behavior, background streaming notices and the current tray-disabled close behavior.
- Preserve single-instance activation, including showing an existing hidden main window. Use compatible instance ownership across old/new executables or explicitly prevent simultaneous access to the same settings/capture session during testing.
- Retain the `ashpd` GlobalShortcuts portal behavior and permission attribution on Wayland. Preserve `org.slopcast.Slopcast` portal identity and desktop metadata separately from the existing `slopcast` settings identity. Replace non-Wayland plugin registration with a native shortcut adapter. Keep conflict/unavailable status and shortcut-recording suppression.
- Replace custom CEF drag/resize and titlebar hacks with GPUI/platform window behavior, checking KDE/GNOME, scaling, maximize/minimize, settings-window reopen and activation. Retain any platform-specific behavior only when still necessary.
- Preserve clipboard actions, folder selection, external URLs, native log rotation/diagnostics and notification sounds currently synthesized with Web Audio.
- Implement explicit shutdown: cancel pending operations/timers, attempt bounded room deletion, preserve failure/quit-anyway semantics, stop publication/capture/audio/metering, unregister callbacks/shortcuts/tray and release runtimes. Hiding a window follows none of these shutdown steps.

**Exit:** tray and shortcuts still control an active stream with no visible window; repeated settings-window cycles do not leak; shutdown releases portal/capture/audio resources; optional-service failures remain visible and recoverable.

**6. Replace packaging and prove an installable release.**

Build a package smoke artifact early in phase 1; finish release packaging here. Provide native staging/install manifests and scripts for Debian packages, AppImage, Linux tar archives and Windows NSIS. Keep the current architecture matrix and artifact contracts until deliberately changed. Packaging tool selection must be validated by building/installing artifacts, not inferred from GPUI's UI support.

Update `package.json`, `.github/workflows/build.yml`, `.github/workflows/release.yml`, desktop-entry scripts and cross-build scripts. Keep the old release target selectable until final cutover. Windows cross-compilation from Linux is currently used: prove it with GPUI or move that job to a native Windows runner, while retaining runtime tests on Windows.

Retain `prepare-gstreamer-runtime.sh` and the media runtime assets. Replace Tauri resource-directory discovery and configure the GStreamer scanner before worker threads start, preserving AppImage-specific lookup. Test installed artifacts on machines without development packages or a globally installed copy of the bundled plugins. Check Windows icon/version/manifest/DPI and C/C++ runtime linking that Tauri's build previously supplied.

Remove the CEF-required dependency check, CEF downloads/caches and CEF-specific bundler patches from the new job. Do not remove Chromium/libwebrtc build requirements or `vendor/webrtc-sys` simply because the UI no longer uses Chromium. Audit the resolved graph for native `links`/GTK version conflicts, Windows binding versions and the existing global `dpi` patch during coexistence.

**Exit:** installed Linux amd64/arm64 and Windows x64 artifacts launch, locate resources, preserve settings/identity, stream to a browser and uninstall correctly. A tar/zip or local `cargo run` alone is not packaging parity.

**7. Cut over and remove the compatibility frontend.**

Make GPUI the default desktop executable after the validation below passes. Keep the previous release artifact available for rollback and retain compatible settings for a release cycle. During side-by-side validation, use isolated settings copies and run only one presenter against physical capture resources at a time.

Then remove `apps/desktop/src-tauri`, the desktop React renderer and browser-only desktop dependencies; Tauri manifests/capabilities/build stamps; the `frame.localhost` transport; Tauri plugins and CEF/winit patches; obsolete CEF automation and packaging paths. Reconcile the final desktop directory/name once the executable is stable. Remove shared packages only if remaining web/server consumers no longer use them.

**Exit:** the production desktop dependency graph and artifacts contain no Tauri, CEF or embedded webview; web/server builds and spectator behavior still pass; documentation and root scripts describe the native desktop path.

## Validation required for cutover

- **Core tests:** reuse existing media tests; port desktop heartbeat, codec, endpoint, audio grouping/targeting and bitrate cases; test settings round trips and deterministic lifecycle races with fake media/API services and clocks.
- **GPUI tests:** use the selected Kit version's `test-support` for input, focus, disabled states, dialogs, multiwindow settings propagation and command dispatch. These tests do not establish native GPU or OS-service correctness. [Upstream testing guide](https://github.com/longbridge/gpui-kit/blob/b2aaa3e5f9a387dea74981586bfca33e7e08420d/website/docs/test.md).
- **End-to-end streaming:** replace the desktop CDP driver in `tests/e2e/desktop/presenter.playwright.ts` with native integration plus a feature-gated test driver into the same core commands, retaining the existing result/handoff contract where useful. Keep Playwright for the browser spectator. Assert received/decoded media, audio continuity and telemetry rather than only presenter counters. Ship no test-control interface in release builds.
- **Real desktop runs:** KDE Wayland and GNOME Wayland; X11 launch/capability behavior; Windows capture/audio; Linux arm64 package smoke; fractional scaling, mixed monitors, source close, portal cancellation/revocation, unsupported shortcuts, absent tray service and window hide/reopen. Exercise representative AMD, Intel and NVIDIA systems available to the project; document untested hardware.
- **Long sessions and failures:** run beyond initial presenter-token validity, interrupt network/server/LiveKit connectivity, reconnect, switch endpoints and capture sources, change settings midstream, and quit during a pending operation. Verify no orphaned room, portal, worker, timer or audio capture where cleanup is possible.
- **Performance:** rerun phase-1 workloads with visible and hidden preview, both static and moving content, and a sustained resize/source-switch loop. Compare CPU/GPU, memory, preview latency and stream drops against agreed budgets. A smaller bundle is useful but does not compensate for capture regressions.

Recommended first implementation change: the phase-1 GPUI spike with real native media linking and preview, plus an integration decision record. Do not start by translating every React component before those feasibility gates are answered.

## Implementation status

Updated 2026-11-06. `packages/desktop-core` and `apps/desktop-gpui` are workspace members; nothing here is committed yet, and the Tauri desktop is unchanged.

### What exists

| Piece | State |
| --- | --- |
| `packages/desktop-core` | Application core: typed room API client, config discovery, settings/preferences storage with the existing files and atomic writes, audio listing/selection/grouping, codec ranking, bitrate recommendation, telemetry smoothing, shortcut parsing, capture adapter over the two native crates, presenter state machine. No Tauri, CEF or GPUI dependency. |
| Presenter | Owns an owned Tokio runtime, publishes `Snapshot`s over a watch channel and `Event`s over a bounded channel. Commands are typed; async completions are tagged with capture/room/endpoint generations so late results cannot revive a stopped session, and resources such requests created are released. |
| `apps/desktop-gpui` | Single main window: room creation, share link/copy, spectators, preview, share/go-live/switch/stop, mute, audio source choice, telemetry, notices and confirmation/source-picker overlays. Reads snapshots; dispatches commands; owns no session state. |
| Preview transport | Native callback → `MediaHub` latest-frame slot → GPUI `RenderImage` → `img(..).object_fit(Contain)`. No HTTP, no JSON, no image encoding. The preview area registers its size in physical pixels and clears the viewport when capture stops. |
| Shutdown | Quit confirmation runs in the core; `QuitNow` closes the window, and an app-quit hook runs bounded room deletion plus native teardown on a background thread. |

### Verified

- `cargo clippy --all-targets` and `cargo fmt --check` clean for both crates; `desktop-core` tests pass (38).
- Both native crates link into the GPUI executable, and the window runs on X11 with a software Vulkan adapter: window creation, theme, snapshot rendering, notices and graceful degradation without PipeWire, a portal or a reachable API endpoint.
- The BGRA preview path renders correctly with preserved aspect ratio and repaints on every delivered frame (checked with an injected test pattern; the hook was removed). A native preview payload uses the same channel order, verified by `parse_preview_payload`'s tests.

### Not verified

- Real capture (Wayland portal, Windows WGC), publication to LiveKit and a browser spectator. This environment has no GStreamer runtime, PipeWire session or LiveKit server, so room creation cannot complete. Phase 1's hardware gates are still open.
- Tray, global shortcuts, single-instance activation, notifications, clipboard beyond the share-link button, and the settings window (stream settings, appearance, typography, shortcuts, onboarding) are not ported.
- No packaging work: `apps/desktop-gpui` has no bundle, desktop entry or GStreamer staging of its own. In development it uses `apps/desktop/src-tauri/resources/gstreamer-plugins` and honours `SLOPCAST_GSTREAMER_DIR`; installed builds look for `resources/gstreamer-plugins` beside the executable.

### Known gaps in the current slice

- Closing the window quits the application, so close-to-tray behaviour and "keep streaming with no window" are not yet exercised.
- Stream settings, endpoint editing, typography and shortcut editing have no GPUI surface yet; the core methods exist, the views do not.
- The Tauri frontend still owns its own presenter behaviour: phase 2's exit criterion (the current desktop running through the extracted core) is not met, and two implementations coexist by design until the GPUI shell reaches parity.
