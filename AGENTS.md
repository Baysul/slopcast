# Room-based native screensharing and web spectator ecosystem

## 1. Project intent and core architecture

This repository contains a room-based screen and audio sharing system with three parts:

1. Desktop client (presenter). The frontend is Tauri 2, React 19, TypeScript 7, Vite 6, Tailwind CSS 4, and shadcn/ui. The backend is pure Rust at `apps/desktop/src-tauri` and links the two Rust workspace crates directly: `native-rust` (the PipeWire/WASAPI capture engine) and `native-livekit` (LiveKit room connection and publishing via the `livekit` Rust SDK and its bundled libwebrtc).
2. Web client (spectator only). A React browser app that lets external users join by room link or code. Web clients can only spectate (receive WebRTC streams). They cannot start screen or audio capture.
3. Signaling and SFU backend server. A Node.js (Express 5) server with a REST API for room creation, code generation (`https://app.domain.com/room/abc-123-xyz`), rate limiting, and LiveKit token issuance. WebRTC signaling is delegated to LiveKit. Its Selective Forwarding Unit (SFU) fans audio and video out to many spectators without overloading the presenter.

## 2. Code quality and linting

Prioritize readability, maintainability, and minimalism. Every line of code is a liability. When in doubt, do less.

### TypeScript, JavaScript, and JSON (Biome)

Follow these rules:

- No `any`, no unnecessary type casts or non-null assertions.
- No speculative abstractions. Don't build for hypothetical future use cases.
- Prefer early returns over nested conditionals. Keep functions small and single-purpose.
- Comments explain why, not what. Remove comments that restate the code, and remove dead or commented-out code.
- Don't extract shared code until it's duplicated 3+ times.
- No silent error handling. No empty catches, no defensive try/catch for impossible cases.
- Don't add libraries, config options, params, or files that aren't needed for the stated goal.
- Match the existing formatting and lint conventions. Don't touch unrelated code.
- Prefer the simplest solution that solves the actual problem, not the most general one.
- Use a ternary only for a trivial single-line assignment with exactly two simple, side-effect-free branches:

```ts
const label = isActive ? 'Active' : 'Inactive';
```

Use if/else, a switch, or a small named function for anything else:

- Nested or chained ternaries (`a ? b : c ? d : e`)
- Ternaries that span multiple lines or need extra parentheses to read
- Ternaries whose branches call functions, mutate state, throw, or have side effects
- Ternaries used for control flow rather than producing a value
- Ternaries where either branch is a complex expression (object literal, multi-element JSX)

This project uses [Biome](https://biomejs.dev/) as the single formatter and linter for all JavaScript, TypeScript, JSX, TSX, and JSON files.

### Rust (rustfmt and clippy)

Follow these rules:

- No reflexive `.clone()` to dodge the borrow checker. Fix the underlying ownership or borrowing instead.
- No `.unwrap()` or `.expect()` on fallible paths in production code. Propagate errors with `?` and proper error types.
- Don't introduce generics, traits, or `dyn` dispatch for a single implementation. Generalize only when a real second use case exists.
- Prefer early returns and `?` over deeply nested `match` or `if let`. Keep functions small and single-purpose.
- Comments explain why, not what. Every `unsafe` block needs a `// SAFETY:` comment. Remove dead or commented-out code.
- Don't extract shared code until it's duplicated 3+ times. Keep the `pub` surface minimal. Prefer `pub(crate)`.
- Don't add crates, config, or files that aren't needed for the stated goal.
- Code must pass `rustfmt` and `cargo clippy --all-targets -- -D warnings`. No blanket lint suppressions. A targeted `#[allow(..., reason = "...")]` is fine.

## 3. Style rules

### Spacing

Group a function body into up to four blocks, in this order, each separated from the next by exactly one blank line. Within a block, no blank lines. Omit any block that doesn't apply. Don't force empty groups.

1. Guards. Early returns and validation, back-to-back with no blank lines between them.
2. Definitions. `let` bindings that compute or gather values used below.
3. Logic. The calls and mutations that do the actual work.
4. Return. The final expression or `Ok(...)`, alone.

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

In the example, the two guards have no blank line between them (same block). The three `let`s that compute the total form one block. The reserve call and receipt construction are logic because they do work rather than compute a value, so they get their own block. `Ok(receipt)` stands alone.

### Naming

- Standard Rust casing: `snake_case` for functions, variables, and modules; `UpperCamelCase` for types, traits, and enums; `SCREAMING_SNAKE_CASE` for consts and statics.
- Names should reveal intent. Avoid generic catch-alls (`data`, `info`, `val`, `tmp`, `thing`). Name what the value represents, not its type or role.
- Booleans get `is_`, `has_`, `should_`, or `can_` prefixes.
- Functions get verb or verb-phrase names that say what they do (`parse_config`, not `config_stuff` or `handle_config`).
- Avoid implementation-shaped type suffixes (`-Impl`, `-Helper`, `-Manager`) unless the suffix does real descriptive work.
- Use the same name for the same concept everywhere. Don't call it `id` in one module and `identifier` in another.
- Single-letter names only in tight, obvious scopes (loop indices, short closures like `|x| x + 1`), never for anything spanning more than a few lines.
- Abbreviate only where Rust or the domain already does (`ctx`, `cfg`, `idx`). Don't invent new ones.

### Expression complexity

- Keep `match`, `if`, and `while` scrutinees simple: a variable, a field access, or one short method call. If producing the value takes more than one chained call, bind it to a named local first.
- Don't nest a multi-line closure inside another expression, whether in a match scrutinee, a chained call, or a function argument. Name it: a local binding, or a function if it's reused. This is usually why `rustfmt` output looks awkward. It formats an inherently tangled expression rather than making a bad choice.

All Rust code must follow the [Rust Style Guide](https://doc.rust-lang.org/stable/style-guide/) and pass `cargo clippy --all-targets -- -D warnings`. The `[lints.clippy]` config in all three Rust crates (`native-rust`, `native-livekit`, `apps/desktop/src-tauri`) enables `pedantic` plus hard `deny` on `unwrap_used`, `expect_used`, `undocumented_unsafe_blocks`, and `allow_attributes_without_reason`.

The `packages/native-rust/rustfmt.toml` and `packages/native-livekit/rustfmt.toml` configs apply 2024 edition defaults with a 100-char line width, import reordering, and module reordering. `apps/desktop/src-tauri` formats with rustfmt defaults.

### Key commands

| Command | Description |
| --- | --- |
| `pnpm check` | Biome CI + `pnpm --recursive check` (per-package gates, see below) |
| `pnpm check:fix` | Apply all safe Biome auto-fixes |
| `pnpm lint` | Run the Biome linter only (read-only) |
| `pnpm lint:fix` | Run the Biome linter and apply safe fixes |
| `pnpm format` | Biome format + `pnpm --filter @slopcast/native-rust rust:fmt` |
| `pnpm rust:fmt` | Run `cargo fmt` on `@slopcast/native-rust` |
| `pnpm rust:check` | Run `cargo fmt --check` on `@slopcast/native-rust` (CI mode) |

### Agent rules

1. Agents must run `pnpm check` after making any code changes and fix all failures before declaring a task complete.
2. All build and package scripts (`build`, `build:desktop`, `package:desktop`, `dist:*`) run `pnpm check` as a prerequisite gate. A build is rejected if `biome ci`, `cargo fmt --check`, or `cargo clippy -- -D warnings` fails.
3. Use the `style` commit type for Biome/rustfmt/clippy-related changes (whitespace, formatting, lint fixes with no logic change).
4. Never override the `biome.json` config per-package. All workspace members are covered by the single root configuration.
5. Never deviate the `rustfmt.toml` config from the Rust Style Guide defaults. Don't add `unstable_features = true` or any configuration that requires nightly.
6. Clippy is enforced by the `check` scripts (`cargo clippy --all-targets -- -D warnings`, inheriting the `[lints.clippy]` config in each Rust crate). Prefer a real fix (renaming, restructure, `// SAFETY:` comment, targeted `#[allow(..., reason = "...")]`) over broad suppressions. Every `unsafe` block needs a `// SAFETY:` comment.

`packages/native-rust` runs `cargo xtask check-targets`, a lightweight Rust binary at `xtask/`, which type-checks the `#[cfg(target_os)]` platform modules the host build cannot see (`cargo check --target ...`). This makes shared-struct/API drift in the Windows module (e.g. E0063) fail locally instead of in CI. Linux hosts check the Windows target (`x86_64-pc-windows-msvc`); Windows hosts check only their own module. The linux target is never cross-checked because `pipewire-sys` and `x11` bind against Linux system headers at build time.

## 4. Tauri security model

The renderer is a sandboxed webview with no Node in it. All privileged work runs in Rust commands, plugin permissions are capability-granted (`src-tauri/capabilities/default.json`), and a strict CSP is set in `tauri.conf.json`. Never bypass the `desktopApi` wrapper, never expose a command that accepts free-form paths or shell input, and never weaken the CSP. Keep the `e2e` cargo feature out of production builds. It opens an unauthenticated localhost Chrome DevTools surface.

## 5. Build and package commands

| Command | Description |
| --- | --- |
| `pnpm dev:desktop` | Run the desktop app in dev mode (`tauri dev`, renderer on :5173) |
| `pnpm dev:web` / `pnpm dev:server` | Web spectator / API server dev servers |
| `pnpm build:desktop` | Runs `pnpm check`, then `pnpm --filter desktop tauri build --no-bundle` (binary only) |
| `pnpm package:desktop` | Runs `pnpm check`, then `pnpm --filter desktop tauri build` (AppImage + deb + nsis) |
| `pnpm dist:desktop` | Build and produce all configured bundles (appimage, deb, nsis) |
| `pnpm dist:desktop:linux` | Build Linux AppImage + deb (`--bundles deb,appimage`) |
| `pnpm dist:desktop:linux:appimage` | Build Linux AppImage only |
| `pnpm dist:desktop:linux:deb` | Build Linux deb only |
| `pnpm dist:desktop:linux:tar` | Build deb, then tarball its payload to `target/release/bundle/Slopcast-<version>-linux-amd64.tar.gz` |
| `pnpm dist:desktop:win` | Build Windows NSIS installer |
| `pnpm test:unit` | Unit tests for the server, shared-types, and desktop renderer |
| `pnpm test:e2e` | Full end-to-end harness (see section 6) |

Artifacts land in `target/release/bundle/{appimage,deb,nsis}/` (the Cargo workspace target dir at the repo root). Build the `e2e` test binary separately with `VITE_E2E=1 pnpm --filter desktop tauri build --features e2e`; add `--no-bundle` when AppImage bundling is unavailable in the environment.

## 6. Automated end-to-end test (`pnpm test:e2e`)

The harness lives at `apps/server/src/e2e-test.ts` and orchestrates two automation phases: a Playwright presenter phase driving the real Tauri binary over CEF's remote-debugging protocol (the `e2e` cargo feature adds the `--remote-debugging-port` flag, script at `apps/desktop/tests/e2e/presenter.playwright.ts`) and a Playwright Chromium spectator phase. It runs one full presenter-to-spectator pass per codec (`E2E_CODECS`, default `h264,h265,vp8,vp9,av1`).

Prerequisites:

```bash
pnpm install
pnpm exec playwright install chromium          # spectator browser
VITE_E2E=1 pnpm --filter desktop tauri build --features e2e   # presenter binary (--no-bundle ok)
# livekit-server must be on PATH (or LIVEKIT_URL must point at a reachable SFU)
pnpm test:e2e
```

What it validates:

| Step | Description |
| :--- | :--- |
| Config and setup | Parses `slopcast.config.json`, kills conflicting port processes, spawns the API server and Web dev server with health polling (30 s timeout). Optionally detects and launches Spotify. |
| LiveKit preflight | TCP-checks the configured `livekitUrl`. For localhost endpoints the harness always runs its own `livekit-server --dev`. A listener on the port is not enough; containerized SFUs often relay signaling but fail ICE/DTLS on the media plane. Anything else fails fast with an actionable error. Also kills stray app instances (`pkill -f target/release/slopcast`) so `tauri-plugin-single-instance` cannot silently hijack the launch. |
| Presenter (Playwright + CEF CDP) | Runs the Playwright script against the `--features e2e` binary with an isolated app config dir (`XDG_CONFIG_HOME` set to `test-output/e2e-userdata`) so real persisted settings cannot leak in. The script asserts the Wayland gate (portal mode only). It clicks "Create Live Room", extracts the room code from `span.font-mono`, starts the screenshare, checks that the preview canvas appears, clicks Go Live, and finds the `[role="status"]` LIVE badge. In synthetic mode (`SLOPCAST_E2E_CAPTURE=synthetic`, the default) the backend feeds a test pattern through the real publish path. No portal picker or Wayland session is needed, and `stream-settings.json` (1080p@60, 20 Mbps, the pass codec) is pre-written for each pass. Telemetry and capture stats are sampled about 2 s apart. `videoFramesEncoded` and `videoBytesSent` must advance, the reported outbound codec must match the pass codec, `previewFramesSent > 0` (the raw-BGRA preview emitter ran), and a published-fps floor is enforced. After the spectator subscribes, a five-second native byte-counter sample must report positive bitrate without exceeding the configured limit by more than the VBR tolerance. GPU diagnostics come from `probe_gpu_info` (a dlopen'd EGL probe replacing `app.getGPUInfo`). `softwareRasterizer` must be false and `eglVendor` must be present. Progress is written to `presenter-phase.json`. The harness polls it and only hands off to the spectator once `handoffReady` flips, then writes a release flag so the script's hold loop (which keeps the app alive during the spectator phase) ends gracefully. |
| Spectator (Chromium) | Headless Chromium via Playwright navigates to the room URL. Asserts the connection badge (`[role="status"]`), a `<video>` element with non-zero dimensions that is actively playing, and in synthetic mode that the received stream is exactly the published 1920x1080 single layer (a halved simulcast layer fails the run). Three receiver `framesDecoded` telemetry samples must have a median of at least 80% of the configured 60 fps. |
| Video capture malfunction checks | Presenter-side: native telemetry must advance and `framesPushed > 0` (catches black-keepalive publishing, where the track is up but capture has died). Spectator-side: two consecutive `requestVideoFrameCallback` frames must arrive (continuous flow, not a single-frame stall) and the decoded pixels must not be uniformly black (64x64 canvas luma/variation check). A `[data-decoder-stalled]` UI flag (codec/profile mismatch) is an error. All of this lands in `e2e-result.json`. |
| Presenter-stop propagation | Mid-hold the harness requests a stop (the harness sets `E2E_STOP_FLAG`; the spec clicks Stop Screenshare, confirms, and acks `E2E_STOPPED_FLAG`). The spectator's `[role="status"]` badge must leave "Live" and report the stream ended while the presenter stays connected. Its room-lifetime audio track keeps the publication alive. The web spectator keys stream liveness off video publications, so a stale "Live" badge fails the run. |
| Diagnostic validation | Parses all console logs for fatal patterns (SEGV, uncaught exceptions, ICE failure, GPU process crash, decoder stall, `framesDecoded=0`). GPU feature status comes from the probe report. Fails if software-rendered. Writes a structured JSON result with pass/fail and per-codec outcomes. |
| Retry and cleanup | `try/finally` guarantees the spectator browser, the presenter session (release flag, graceful teardown, SIGTERM), and spawned servers are shut down and ports reaped even when a step throws. Console logs and the result JSON are written on every outcome. The whole run retries up to 2 attempts with a root-cause summary after all attempts are exhausted, and exits non-zero if every attempt fails. |

Output artifacts (written to `test-output/`):

| File | Content |
| :--- | :--- |
| `desktop-console.log` | All captured console entries from every source (server, web, presenter script/tauri backend logs, livekit, spectator) |
| `web-console.log` | All DevTools console output from the Chromium spectator |
| `desktop-gpu-report.json` | Raw `probe_gpu_info` output: EGL vendor, GL renderer/version, software-rasterizer flag |
| `presenter-phase.json` | Structured presenter-phase result (room code, telemetry, GPU report, errors) written by the Playwright presenter script |
| `e2e-result.json` | Structured `TestResult`: room code, video dimensions, per-codec results, GPU status, errors, duration, retries |
| `e2e-userdata/` | Isolated app config dir (stream-settings.json per pass, onboarding state) |

Agent rules for the e2e test:

1. The test scripts (`apps/server/src/e2e-test.ts`, `apps/desktop/tests/e2e/presenter.playwright.ts`) must pass `pnpm check` before being considered complete.
2. The `biome.json` override for `e2e-test.ts` allows `noExplicitAny` for Playwright locator chains. Don't remove it without verifying the test still passes Biome CI.
3. When adding new UI elements to the desktop or web app, update the Playwright selectors in `presenter.playwright.ts` and the assertions in `e2e-test.ts` to match (`span.font-mono` room code, `[role="status"]` badges, `[data-decoder-stalled]`, preview canvas).

## 7. Manual walkthrough for room creation and web spectators

1. One-time dev setup: run `pnpm desktop:install-entry` so the Wayland taskbar shows the real app icon for dev builds.
2. Start LiveKit in dev mode: `docker run --rm -p 7880:7880 -p 7881:7881 -p 7882:7882/udp livekit/livekit-server --dev --bind 0.0.0.0` (or `docker compose up -d livekit`).
3. Start the server: `pnpm dev:server`. Web client: `pnpm dev:web`. Desktop app: `pnpm dev:desktop`.
4. Click "Create Room" in the Desktop app, then copy the generated link (`http://localhost:3000/room/XYZ123`).
5. Open the link in a browser (Chrome/Firefox) and verify the Web client auto-connects as Spectator.
6. Start screenshare in the Desktop app and verify high-fps video and the exclusive window audio stream in the Web browser.

### Testing PipeWire exclusive window audio on Linux

Run these verification commands:

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

## 8. Key dependency versions

| Package | Version |
| --- | --- |
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
| Playwright | ^1.55.0 |
| Playwright | ^1.55.0 |
| Node.js (required) | >= 24.0.0 (pnpm 9.15.4, `.nvmrc`) |

## 9. Git commit conventions

All commits must follow [Conventional Commits](https://www.conventionalcommits.org/) with a strict type prefix.

### Commit format

```
type: short description (max 72 chars)

Optional body with additional context. Blank line between
subject and body is required when body is present.
```

### Allowed types

| Type | When to use |
| --- | --- |
| `feat` | A new feature or user-facing capability |
| `fix` | A bug fix |
| `refactor` | A code change that is neither a fix nor a feature |
| `chore` | Build, tooling, dependency updates, or CI changes |
| `docs` | Documentation-only changes (README, AGENTS.md, etc.) |
| `test` | Adding or updating tests |
| `style` | Formatting, whitespace, lint fixes (no logic change) |
| `perf` | Performance improvement |

### Rules

1. First line is the commit message. Keep it under 72 characters, lowercase after the type prefix, no trailing period.
2. Body is optional. Include it when the subject alone is insufficient. Wrap at 72 characters.
3. One logical change per commit. Avoid combining unrelated changes.
4. Stage only intended files. Run `git status` and `git diff` before committing to verify.
5. Never commit secrets, API keys, or large generated assets.
6. Don't commit `.opencode/` or other tool-specific config directories unless explicitly asked.

### Examples

```
feat: add room code generation with nanoid

fix: resolve PipeWire audio PIDs via /proc fallback

refactor: extract AudioAppPicker into shared component

chore: bump Tauri to v2.11

docs: add git commit conventions to AGENTS.md
```
