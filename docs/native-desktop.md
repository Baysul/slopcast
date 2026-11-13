# Native desktop

The desktop executable is a Rust GPUI Kit application. The former Tauri backend, React desktop renderer, CEF runtime, IPC frame server and browser automation have been removed. The API server and spectator website retain their browser stack.

## Code and ownership

- `apps/desktop`: executable `slopcast`, native views, settings window, platform services, logging and resources.
- `packages/desktop-core`: typed presenter commands, room/heartbeat lifecycle, settings storage, audio selection, telemetry and the latest-frame handoff. It has no UI framework dependency.
- `packages/native-livekit` and `packages/native-rust`: the existing capture, encoding, publication and audio engines. libwebrtc remains a media dependency; it is not an embedded browser runtime.

The process owns one presenter and one runtime. Main and settings windows observe the same snapshot. Buttons, tray items and global shortcuts dispatch the same actions. Single-instance ownership is scoped to the settings identity; a second launch activates the existing presenter instead of opening another capture session.

Preview frames pass directly from the native callback through `MediaHub` into GPUI. Frames are validated as tightly packed BGRA, coalesced to the newest frame, scaled in physical pixels and rendered with preserved aspect ratio. Replaced and cleared images are explicitly removed from the sprite atlas. Hidden/closed windows clear the preview viewport, and showing the window restores it without restarting publication.

## Build and run

```bash
pnpm install
pnpm build:desktop
./target/release/slopcast
```

`pnpm dev:desktop` prepares the Linux media runtime and launches the debug native executable. `cargo run -p slopcast` can be used directly when the runtime is already prepared. There is no desktop development web server.

Linux builds need a working Vulkan driver at runtime, X11/Wayland and xkbcommon, fontconfig/FreeType, PipeWire, the GStreamer development/runtime packages, a C++ compiler compatible with the vendored libwebrtc headers, and `pkg-config`. CI records the complete apt dependency list. Desktop capture uses the Wayland portal; X11 still launches for audio, UI smoke tests and the synthetic test source.

The pinned GPUI Kit version is `0.9.0`. Windows builds require a Windows host with the MSVC toolchain and Windows SDK: GPUI compiles its release DirectX shaders with the SDK's `fxc.exe`. Windows CI therefore requires a native Windows runner.

## Settings and desktop services

Existing settings remain compatible: `stream-settings.json`, `onboarding.json`, `desktop-preferences.json` and `settings-location.json` retain their schemas. The identity directory remains `~/.config/slopcast` on Linux and `%APPDATA%\slopcast` on Windows. `SLOPCAST_SETTINGS_DIR` isolates a development/test instance.

The main view retains the Tauri layout: a 40 px custom title bar, a centered 16:9 preview with 24 px padding, and a 352 px sidebar with source and audio sections. Telemetry overlays the live preview. All five themes retain their original light and dark palettes.

Sharing is one action. **Start sharing** (button, tray item or global shortcut) creates a room when there is none, opens the source picker (the ScreenCast portal on Linux, the in-app list on Windows) and publishes as soon as the first frame arrives. The `Previewing` capture stage is only the short gap between picking and publishing. Dismissing the picker cancels and keeps the room open. **Create room only** opens a room without sharing, so the link can be sent first. Before opening the portal, Linux checks the portal's `AvailableSourceTypes`. A value of zero means `xdg-desktop-portal` has no ScreenCast backend for the session (for example, it was started under another desktop), and the app reports this rather than failing silently.

Audio rows show each application's icon. On Linux the process is matched to a desktop entry by executable, application name, Steam app id or systemd scope; that entry's `Icon=` is then looked up in the current icon theme (`kdeglobals` on KDE, otherwise `gsettings`), falling back to `hicolor` and `pixmaps`.

Settings use the original General, Appearance, Stream and Shortcuts tabs in a native window: endpoint validation and room replacement/defer controls, resolution/FPS/codec/bitrate, automatic audio, tray preference, typography, appearance, settings relocation and keyboard bindings. Endpoint changes preserve the core's health and room cleanup checks. Bundled fonts retain their OFL licenses. GPUI selects static weight faces from the checked-in `.ttc` collections; regenerate them from the original variable fonts with `uv run --with fonttools python scripts/prepare-desktop-fonts.py`.

Linux tray support uses the StatusNotifier/D-Bus protocol; it requires a desktop tray host (or the appropriate shell extension). Windows uses a native icon/menu. A successful tray registration permits closing the main window while the presenter continues. If the tray is unavailable or disabled, closing requests the ordinary quit confirmation. Losing the tray while the main window is closed reopens the window.

Wayland global shortcuts use the GlobalShortcuts portal under `org.slopcast.Slopcast`. The desktop can ask for approval, alter the requested bindings or reject registration. X11 and Windows use native global shortcuts. Registration failures are visible in Settings and can be retried. `Ctrl+,` opens Settings; the saved telemetry shortcut toggles the stream panel. Escape dismisses confirmations and the source picker.

Logs are mirrored to stderr and saved under the platform's local data directory, `slopcast/logs/app.log`. The previous log is rotated to `app.log.1` on startup when it exceeds 5 MB.

## Media resources and distribution

Linux preparation stages the stock GStreamer plugins and scanner in `apps/desktop/resources/gstreamer-plugins`. Installed binaries look for `resources/gstreamer-plugins` beside the executable. Direct binaries in `target/debug` or `target/release` can use the development resource directory. `SLOPCAST_GSTREAMER_DIR` overrides discovery; the plugin scanner is selected before native worker threads start.

```bash
pnpm dist:desktop:linux:deb
pnpm dist:desktop:linux:appimage
pnpm dist:desktop:linux:tar
pnpm dist:desktop:win
```

Linux packages support amd64 and arm64. Debian/tar payloads install the executable and media resources under `usr/lib/slopcast`, with a relocatable launcher in `usr/bin`. AppImage includes linked binary/plugin dependencies while using the host's libc and graphics drivers; set `APPIMAGETOOL` to an appimagetool executable if it is not on PATH. The tar payload has the same distro runtime dependencies as the Debian package.

Windows packaging uses NSIS and installs per user, with a native icon, version information and DPI manifest. Uninstalling removes application files and the shortcut, retaining the user's settings. Artifacts remain under `target/release/bundle` on Linux and `target/x86_64-pc-windows-msvc/release/bundle/nsis` for CI's explicit Windows target.

## Validation

`pnpm check` runs repository formatting, lint/type checks and the Rust checks. `python3 scripts/check-desktop-deps.py` rejects embedded browser shell dependencies in the desktop graph for all target platforms.

```bash
cargo build --locked -p slopcast --features e2e
python3 scripts/check-desktop-xvfb.py
```

The Xvfb smoke script exercises the actual native window and synthetic media callback path: colors/paint, modal input blocking and Escape, mute/global shortcut dispatch, second-launch activation, native settings, API dispatch, sustained preview memory, resize, hidden-preview suppression, stop and quit. Screenshots/logs go to `test-output/native-smoke`.

Live streaming tests build the same executable with `--features e2e`, then use `pnpm test:e2e`. The feature-gated native driver sends typed commands and writes the existing presenter phase/release/stop files; Playwright is used only for the browser spectator. Native GPU specifications and transmitted/decoded media are checked. Ordinary builds include no test driver, browser debugging port or remote control interface.

For an isolated software-rendered diagnostic run, set `E2E_ALLOW_SOFTWARE=1`; hardware validation rejects software rendering by default. `E2E_CODECS`, `E2E_RESOLUTION`, `E2E_FPS` and `E2E_BITRATE_LIMIT` can select a smaller streaming pass.

Software-rendered Xvfb checks do not establish real Wayland portal behavior, desktop tray/shortcut approval, Windows device capture, hardware encoder performance or installed-package behavior on every distribution. CI keeps Linux amd64/arm64 packaging and native Windows builds separate; real desktop streaming remains a release validation requirement.
