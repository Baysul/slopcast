# Slopcast

<p align="center">
  <img src="apps/desktop/resources/icon.svg" alt="Slopcast" width="128">
</p>

Cross-platform room-based screen and audio sharing — present from the desktop app, spectate from any browser[^1].

<p align="center">
  <img src="screenshot.png" alt="Slopcast screenshot" width="720">
</p>

## Heads-up

This a vibe-coded, personal project of mine that I plan to maintain until the day *an actually good app* for sharing your screen on Linux/Wayland appears.

### Why not user other apps?
- **Discord:** A Nitro subscription is required for high-bitrate video, and a frame-rate higher than 30fps. Not to mention that on my system it captures my entire desktop's audio instead of solely the audio from the window I selected. Streams were often super pixel-y also, especially when there was a lot going on.
- **Vesktop:** Streams just wouldn't start for me, and the app in general was buggy. I also don't like the idea of using a third-party Discord client. It violates Discord's terms of service and has direct access to sensitive conversations.
- **Element:** As of now has no support for screen sharing with audio, and lacks settings to adjust the bitrate, resolution and framerate of the stream. [There's a pending pull request that was made in February of this year](https://github.com/element-hq/element-call/pull/3736#issuecomment-4845070478), but a merge is nowhere in sight. That's not to mention the possible overhead investment of [setting up a homeserver](https://element-hq.github.io/synapse/latest/welcome_and_overview.html) with Element Call - which means a well-configured, resource-hungry Synapse server (+ web server, + PostgreSQL server), a TURN server and LiveKit deployment .. that you then have to convince your friends to register on and use.
- **Jitsi Meet:** Honestly, it's not terrible. The bitrate is good and it's fairly straightforward to create a room and have your friends join. I don't remember what exactly issue I had, but I think it could only share tabs or something, and the audio and latency weren't in general weren't great.

### What makes a good screensharing app, anyway?

Well first of all, native Linux desktop support is a must. The app should:
- Run natively on Wayland
  - This usually means being PipeWire-aware and making use of PipeWire introspection and other techniques **to automatically capture a single application's audio**, rather than the entire desktop's. Just like how Discord does on Windows.
  - Usage of [XDG Desktop Portal's](https://docs.flatpak.org/en/latest/desktop-integration.html#portals) application picker for a smooth experience across different environments.
- Make use of hardware-accelerated video encoding when available.
  - So far this has only been tested on a system with an RDNA2 GPU with the open-source driver.
  - If you use NVIDIA: [let me know if you run into any issues](https://github.com/Baysul/slopcast/issues). Install whatever driver makes sense for your GPU[^3] and then install `nvidia-utils`.
- Be free and open-source.
- Be easy to use. You and your friends should be able to watch a movie together with as little friction as possible. The UI must also be relatively simple and approachable while still looking good enough.
- Not require registration or a download for people who just need to watch.[^2]
- A cross-platform desktop app
  - This is still a work in progress! Windows and Mac builds haven't been tested yet, but a functioning Windows build will be coming soon.

### 


## How it works

1. **Present** — Launch the desktop app, create a room, and share your screen with per-application audio capture.
2. **Spectate** — Share the room link. Anyone opens it in a browser and watches the stream instantly — no install, no account.

The desktop app captures exactly one target application's audio via native OS APIs (PipeWire on Linux). No other app's sound ever leaks into the stream.

## Development

```bash
pnpm install
pnpm build:desktop

# Terminal 1 — signaling server
pnpm dev:server

# Terminal 2 — web spectator
pnpm dev:web

# Terminal 3 — desktop presenter
pnpm dev:desktop
```

Click **Create Live Room** in the desktop app, copy the room link, and open it in a browser.

## Configuration

For local development, configuration is loaded from `slopcast.config.json` at the project root
with LiveKit defaults pointing to `ws://localhost:7880`. In production, set the environment
variables below to override each value — they take precedence over the config file.

### Environment variables

| Variable | Description | Default |
|---|---|---|
| `SERVER_PORT` | API server port | `3001` |
| `WEB_PORT` | Web client port | `3000` |
| `API_ENDPOINT` | URL the web client uses to reach the API | `http://localhost:3001` |
| `WEBSITE_URL` | Public web app URL (used in generated share links) | `http://localhost:3000` |
| `LIVEKIT_URL` | LiveKit server WebSocket URL | `ws://localhost:7880` |
| `LIVEKIT_API_KEY` | LiveKit API key | `devkey` |
| `LIVEKIT_API_SECRET` | LiveKit API secret | `secret` |

## Deployment

### Docker Compose

The repository includes a `docker-compose.yml` that runs both the API server and the web client:

```bash
# Set your LiveKit credentials
export LIVEKIT_API_KEY=your-api-key
export LIVEKIT_API_SECRET=your-api-secret

# Start both services
docker compose up -d
```

The API server is exposed on port `3001`, the web client on port `3000`. By default, the web
container's `API_ENDPOINT` points to the server container directly.

### Reverse proxy (production)

For production, put a reverse proxy in front to serve both the web SPA (static files) and the API
server. The examples below use a single domain like `app.example.com`.

#### Nginx

Place the built web app files at `/var/www/slopcast-web` (or mount them from the
`slopcast-web` container) and create `/etc/nginx/nginx.conf`:

```nginx
http {
  server {
    listen 80;
    server_name app.example.com;

    root /var/www/slopcast-web;
    index index.html;

    location / {
      try_files $uri $uri/ /index.html;
    }

    location /api/ {
      proxy_pass http://127.0.0.1:3001;
      proxy_set_header Host $host;
      proxy_set_header X-Real-IP $remote_addr;
      proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
      proxy_set_header X-Forwarded-Proto $scheme;
    }

    location /health {
      proxy_pass http://127.0.0.1:3001;
      proxy_set_header Host $host;
      proxy_set_header X-Real-IP $remote_addr;
    }

    location /ws {
      proxy_pass http://127.0.0.1:3001;
      proxy_http_version 1.1;
      proxy_set_header Upgrade $http_upgrade;
      proxy_set_header Connection "upgrade";
      proxy_set_header Host $host;
    }
  }
}
```

For HTTPS, add `ssl` and redirect port 80:

```nginx
server {
  listen 443 ssl http2;
  server_name app.example.com;
  # ssl_certificate /etc/nginx/ssl/cert.pem;
  # ssl_certificate_key /etc/nginx/ssl/key.pem;
  # ... same location blocks as the HTTP example above
}

server {
  listen 80;
  server_name app.example.com;
  return 301 https://$host$request_uri;
}
```

#### Caddy

Create a `Caddyfile`:

```
app.example.com {
  root * /var/www/slopcast-web
  try_files {path} /index.html
  file_server

  reverse_proxy /api/* 127.0.0.1:3001
  reverse_proxy /health 127.0.0.1:3001
  reverse_proxy /ws 127.0.0.1:3001
}
```

Caddy provisions TLS certificates automatically via Let's Encrypt / ZeroSSL when the
domain's DNS points to the server. No extra HTTPS configuration is needed.

To run Caddy in Docker with the Docker Compose setup (uncomment the `caddy` service in
`docker-compose.yml`) and place the `Caddyfile` alongside:

```yaml
# docker-compose.yml additions:
volumes:
  - ./Caddyfile:/etc/caddy/Caddyfile:ro
  - caddy_data:/data
  - caddy_config:/config
```

## Project structure

```
apps/
├── desktop/     Electron + React + Rust (presenter & spectator)
├── web/         React browser app (spectator-only)
└── server/      Express + WebSocket signaling server
packages/
├── native-rust/ napi-rs native engine (PipeWire audio capture)
└── shared-types/ Shared TypeScript interfaces
```

## Requirements

### All platforms

- **Node.js** >= 24.0.0
- **pnpm** >= 9

### Desktop app

| Platform | Requirements |
|----------|-------------|
| **All** | [Rust toolchain](https://rustup.rs) (stable), C++20 compiler |
| **Linux** | [PipeWire](https://pipewire.org/) (`libpipewire-0.3-dev`), `xdg-desktop-portal` (Wayland) |
| **macOS** | Xcode Command Line Tools (`xcode-select --install`) |
| **Windows** | MSVC 2022+ (Build Tools for Visual Studio) |

The native Rust module (`packages/native-rust`) is compiled via napi-rs and linked into the Electron main process. A C++20-capable toolchain is required (gcc >= 10, clang >= 10, or MSVC 2022+).

### Server & Web

Both run on Node.js with no additional native dependencies. Playwright (Chromium) is required for running the end-to-end test suite:

```bash
pnpm exec playwright install chromium
```

---

[^1]: Currently tested on Chromium-based browsers (Chrome, Edge, Brave, Opera) and Firefox.

[^2]: The presenter still has to download the app, but a spectator **does not.**

[^3]: https://wiki.archlinux.org/title/NVIDIA#Installation