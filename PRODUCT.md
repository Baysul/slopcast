# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

**Primary:** Gamers who want to stream their gameplay with exclusive per-application audio capture — sharing only their game's audio while keeping Discord chat, music, and other apps' sound private and off-stream.

**Secondary:** Developers, designers, and remote collaborators who need low-latency screen sharing with per-window audio isolation during pair programming, demos, or feedback sessions.

**Tertiary:** Any user who wants to spectate a live screen share instantly via browser — no install, no account, just a room code.

## Product Purpose

Slopcast is an open-source, room-based screen and audio sharing ecosystem that enables a presenter to broadcast high-fps video and exclusive per-application audio to unlimited web-based spectators via WebRTC. It exists to solve the problem that existing tools (Discord, Zoom, Google Meet) either cannot isolate audio per-application, require spectators to install software, or are proprietary SaaS platforms the user cannot self-host.

## Positioning

An open-source, self-hostable screenshare ecosystem that gives presenters surgical control over which application's audio is streamed — nothing else leaks. Unlike Discord's "entire desktop audio" approach, Slopcast captures exactly one target application's sound. Unlike SaaS tools, the entire stack (signaling server, desktop presenter client, web spectator UI) is deployable on the user's own infrastructure with no accounts or subscriptions.

## Operating Context

- **Presenter** launches the Desktop App, creates a room, selects a window to share, and optionally overrides the auto-detected audio source. Audio capture happens through native OS audio APIs (PipeWire on Linux, WASAPI on Windows).
- **Spectators** open a browser URL or enter a room code on the Web App. They are strictly read-only: they receive video + filtered audio tracks via WebRTC. No install, no account.
- **Rooms are ephemeral.** Created on demand, identified by a unique room code, and closed when the presenter disconnects. No persistent storage of streams.
- A **Signaling Server** manages WebSocket connections, room state, and WebRTC signalling between presenter and spectators.

## Capabilities and Constraints

- **Desktop App (Tauri 2):** Screenshare capture, per-window audio capture (virtual capture sink linked only to target app), room creation, WebRTC broadcasting to multiple spectators.
- **Web App (React):** Room join by code/link, WebRTC video+audio reception, spectator-only enforcement (no publish capability), connection state management.
- **Signaling Server (Express + ws):** Room creation with unique code, participant tracking, role assignment, WebRTC signaling relay (offers/answers/ICE candidates).
- **Native Rust Engine (`packages/native-rust` + `native-livekit`):** PipeWire graph control for virtual capture sink creation and target-only audio linking on Linux; WASAPI process loopback on Windows; LiveKit room + publishing.
- **Key constraints:**
  - Web client cannot publish media streams (enforced by design and protocol).
  - Audio capture on Linux depends on PipeWire; KDE windows lack identity metadata for auto-detection.
  - Rooms are single-presenter; only one stream per room.
  - Windows native audio driver is in progress.

## Brand Commitments

- Product name: **Slopcast**
- Author: Basil <parsley@duck.com>
- Open-source (no accounts, self-hostable)
- No existing brand assets, logo, colors, or typography — visual identity is to be established from scratch.

## Evidence on Hand

- **Working code:** Linux PipeWire audio capture (`packages/native-rust/src/linux/`), WebRTC signaling (`apps/server/`), web spectator UI (`apps/web/`), desktop presenter UI + Tauri backend (`apps/desktop/`).
- **README/AGENTS.md:** Extensive product and architecture documentation.
- **No design assets exist.** No DESIGN.md, no logo, no color tokens, no brand guidelines.

## Product Principles

1. **Audio privacy is non-negotiable.** The system must guarantee that only the target application's audio is streamed. No other audio ever leaks into the broadcast.
2. **The web spectator experience must be instant.** No install, no account, no friction — a room link opens directly into a live stream.
3. **Open-source and self-hostable.** Every component must be independently deployable. No dependency on proprietary infrastructure.
4. **Low latency, high fidelity.** Native audio capture and direct WebRTC to minimize delay. Favor native OS APIs over browser-only approaches.
5. **Singular focus on screenshare.** This product does one thing well. It is not a chat app, not a meeting platform — it is a screenshare and spectating ecosystem.

## Accessibility & Inclusion

No product-specific accessibility requirements have been established. The web UI should follow standard WCAG practices.
