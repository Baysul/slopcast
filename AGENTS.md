# AGENTS.md — Room-Based Native Screensharing & Web Spectator Ecosystem

## Project Intent & Core Architecture

This repository contains a modern, room-based screen and audio sharing system. The ecosystem consists of three primary components:

1. **Desktop Client (Presenter):** Built with **Tauri 2 + React 19 + TypeScript 7 + Vite 6 + Tailwind CSS 4 + shadcn/ui** for the frontend, and a pure-Rust backend (`apps/desktop/src-tauri`) that links the two Rust workspace crates directly — **`native-rust`** (PipeWire/WASAPI capture engine) and **`native-livekit`** (LiveKit room connection + publishing via the `livekit` Rust SDK and its bundled libwebrtc).
2. **Web Client (Spectator-Only):** A lightweight React browser application allowing external users to join room links/codes instantly. **Web clients are strictly restricted to spectating (receiving WebRTC streams)** and cannot initiate screen or audio capture.
3. **Signaling & SFU Backend Server:** Node.js (Express 5) server exposing a REST API for room creation, code generation (`https://app.domain.com/room/abc-123-xyz`), rate limiting, and LiveKit token issuance. WebRTC signaling itself is delegated to LiveKit, whose Selective Forwarding Unit (SFU) handles audio/video fan-out to support multi-spectator rooms without overloading presenters.

## Code Quality & Linting

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

This project uses [Biome](https://biomejs.dev/)  as the unified formatter and linter for all JavaScript, TypeScript, JSX, TSX, and JSON files.

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

**Agent Rules:**
1. Agents MUST run `pnpm check` after making any code changes and fix all failures before declaring a task complete.
2. All build and package scripts (`build`, `build:desktop`, `package:desktop`, `dist:*`) run `pnpm check` as a prerequisite gate; a build is rejected if `biome ci`, `cargo fmt --check`, or `cargo clippy -- -D warnings` fails.
3. Use the `style` commit type for Biome/rustfmt/clippy-related changes (whitespace, formatting, lint fixes with no logic change).
4. The `biome.json` config must never be overridden per-package. All workspace members are covered by the single root configuration.
5. The `rustfmt.toml` config must never deviate from the Rust Style Guide defaults. Do not add `unstable_features = true` or any configuration that requires nightly.
6. Clippy is enforced by the `check` scripts (`cargo clippy --all-targets -- -D warnings`, inheriting the `[lints.clippy]` config in each Rust crate). Prefer a real fix (renaming, restructure, `// SAFETY:` comment, targeted `#[allow(..., reason = "...")]`) over broad suppressions; every `unsafe` block needs a `// SAFETY:` comment.

**Multi-platform Rust type check:** `packages/native-rust` runs `cargo xtask check-targets` (a lightweight Rust binary at `xtask/`) which type-checks the `#[cfg(target_os)]` platform modules the host build cannot see (`cargo check --target …`), so shared-struct/API drift in the Windows module (e.g. E0063) fails locally instead of in CI. Linux hosts check the Windows target (`x86_64-pc-windows-msvc`); Windows hosts check only their own module. The linux target is never cross-checked because `pipewire-sys`/`x11` bind against Linux system headers at build time.

---
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

---
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
| **Presenter (WebdriverIO + Tauri)** | Runs the WDIO spec against the `--features e2e` binary with an isolated app config dir (`XDG_CONFIG_HOME` → `test-output/e2e-userdata`) so real persisted settings cannot leak in. The spec: asserts the Wayland gate (portal mode only), clicks "Create Live Room" and extracts the room code from `span.font-mono`, starts the screenshare → preview canvas appears → Go Live → `[role="status"]` LIVE badge. In synthetic mode (`SLOPCAST_E2E_CAPTURE=synthetic`, the default) the backend feeds a test pattern through the real publish path — no portal picker, no Wayland session needed; `stream-settings.json` (1080p@60, 20 Mbps, the pass codec) is pre-written for each pass. Telemetry + capture stats are sampled ~2 s apart: `videoFramesEncoded`/`videoBytesSent` must advance, the reported outbound codec must match the pass codec, `previewFramesSent > 0` (raw-BGRA preview emitter ran), and a published-fps floor is enforced. After the spectator subscribes, a five-second native byte-counter sample must report positive bitrate without exceeding the configured limit by more than the VBR tolerance. GPU diagnostics come from `probe_gpu_info` (dlopen'd EGL probe replacing `app.getGPUInfo`): `softwareRasterizer` must be false and `eglVendor` present. Progress is written to `presenter-phase.json`; the harness polls it and only hands off to the spectator once `handoffReady` flips, then writes a release flag so the spec's hold test (which keeps the app alive during the spectator phase) ends gracefully. |
| **Spectator (Chromium)** | Headless Chromium via Playwright navigates to the room URL. Asserts the connection badge (`[role="status"]`), a `<video>` element with non-zero dimensions that is actively playing, and — in synthetic mode — that the received stream is **exactly the published 1920x1080 single layer** (a halved simulcast layer fails the run). Three receiver `framesDecoded` telemetry samples must have a median of at least 80% of the configured 60 fps. |
| **Video capture malfunction checks** | Presenter-side: native telemetry must advance AND `framesPushed > 0` (catches black-keepalive publishing: track up, capture dead). Spectator-side: two consecutive `requestVideoFrameCallback` frames must arrive (continuous flow, not a single-frame stall) and the decoded pixels must not be uniformly black (64x64 canvas luma/variation check). A `[data-decoder-stalled]` UI flag (codec/profile mismatch) is treated as an error. All recorded in `e2e-result.json`. |
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
