# Slopcast

<p align="center">
  <img src="apps/desktop/resources/icon.svg" alt="Slopcast" width="128">
</p>

<p align="center">
  Share a screen and one app's audio from the desktop app. Spectators open a link in a browser, with no account and nothing to install.
</p>

<p align="center">
  <img src="screenshot.png" alt="Slopcast screenshot" width="720">
</p>

## Run it

You need:

- Node.js 24 or newer and pnpm 9 or newer.
- The [Rust toolchain](https://rustup.rs) and a C++20 compiler (gcc 10+, clang 10+, or Build Tools for Visual Studio 2022).
- On Linux: PipeWire (`libpipewire-0.3-dev`), X11 (`libx11-dev`), GStreamer development packages, the `gstreamer1.0-plugins-bad`, `gstreamer1.0-plugins-ugly` and `gstreamer1.0-vaapi` plugins, `xdg-desktop-portal`, `pkg-config`, and `clang`.

From the repository root, install dependencies and build the desktop app once:

```bash
pnpm install
pnpm build:desktop
```

Start a LiveKit server in its own terminal. The `--dev` flag uses the default key and secret that `slopcast.config.json` already expects:

```bash
docker run --rm -p 7880:7880 -p 7881:7881 -p 7882:7882/udp \
  livekit/livekit-server --dev --bind 0.0.0.0
```

Start the API server, the web client, and the desktop app, each in its own terminal:

```bash
pnpm dev:server
pnpm dev:web
pnpm dev:desktop
```

Click **Create Live Room** in the desktop app and copy the link. Anyone who opens it watches in a browser.

## Background

Slopcast is a vibe-coded personal project. I plan to maintain it until *an actually good app* for sharing a screen on Linux and Wayland appears.

### Why not use other apps?

- **Discord:** A Nitro subscription is required for high-bitrate video, and a frame-rate higher than 30fps. Not to mention that on my system it captures my entire desktop's audio instead of solely the audio from the window I selected. Streams were often super pixel-y also, especially when there was a lot going on.
- **Vesktop:** Streams just wouldn't start for me, and the app in general was buggy. I also don't like the idea of using a third-party Discord client. It violates Discord's terms of service and has direct access to sensitive conversations.
- **Element:** As of now has no support for screen sharing with audio, and lacks settings to adjust the bitrate, resolution and framerate of the stream. [There's a pending pull request that was made in February of this year](https://github.com/element-hq/element-call/pull/3736#issuecomment-4845070478), but a merge is nowhere in sight. That's not to mention the possible overhead investment of [setting up a homeserver](https://element-hq.github.io/synapse/latest/welcome_and_overview.html) with Element Call, which means a well-configured, resource-hungry Synapse server (plus a web server and PostgreSQL), a TURN server and LiveKit deployment, and then you have to convince your friends to register and use it.
- **Jitsi Meet:** Honestly, it's not terrible. The bitrate is good and it's fairly straightforward to create a room and have your friends join. I don't remember what exactly the issue was, but I think it could only share tabs or something, and the audio and latency weren't great in general.

### What makes a good screen-sharing app?

Native Linux desktop support is non-negotiable. A good app should:

- Run natively on Wayland.
  - Use PipeWire introspection to capture one application's audio instead of the entire desktop, as Discord does on Windows.
  - Use the [XDG Desktop Portal](https://docs.flatpak.org/en/latest/desktop-integration.html#portals) picker across desktop environments.
- Use hardware-accelerated video encoding when available.
  - Slopcast has been tested on an RDNA2 GPU with the open-source driver.
  - NVIDIA users should install the appropriate driver for their GPU[^3] and `nvidia-utils`. [Open an issue](https://github.com/Baysul/slopcast/issues) if you run into problems.
- Be free and open source.
- Make it easy for friends to watch a movie together. The interface should stay simple and approachable.
- Let spectators watch without registering or downloading an app.[^2]
- Provide desktop apps for Linux and Windows.

[^2]: The presenter still has to download the app, but a spectator **does not.**

[^3]: https://wiki.archlinux.org/title/NVIDIA#Installation
