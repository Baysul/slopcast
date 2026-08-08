---
target: apps/desktop/src/renderer/main.tsx
total_score: 25
max_score: 40
na_heuristics: 
p0_count: 0
p1_count: 3
p2_count: 3
timestamp: 2026-07-30T21-12-50Z
slug: apps-desktop-src-renderer-main-tsx
---
# Design Critique: Slopcast Desktop Presenter UI

`apps/desktop/src/renderer/main.tsx` — 2174-line monolithic React component for the Electron presenter app.

---

## Design Health Score

| # | Heuristic | Score | Key Issue |
|---|-----------|-------|-----------|
| 1 | Visibility of System Status | **3** | Connection-quality degradation signaled only by subtle telemetry color changes; no reconnection indicator, no disconnect toast |
| 2 | Match System / Real World | **3** | "On Air"/"LIVE" terminology maps well to streamers, but codec labels lack scenario guidance (which codec for what use?) |
| 3 | User Control and Freedom | **3** | Mid-stream settings changes are excellent, but no "Leave Room" separate from "Stop Screenshare" and no audio-source undo |
| 4 | Consistency and Standards | **2** | Raw Tailwind gray utilities mixed with CSS-variable tokens; DESIGN.md card spec (backdrop-blur, surface-80%) unused; 6 font sizes vs. defined 4-step scale |
| 5 | Error Prevention | **3** | "Stop Screenshare" has no confirmation dialog; "Create Live Room" lacks disabled/loading state to prevent double-clicks |
| 6 | Recognition Rather Than Recall | **3** | Audio picker with live meters and X11 thumbnails aids recognition, but Wayland picker disappears entirely into static text |
| 7 | Flexibility and Efficiency | **2** | Zero keyboard shortcuts — every action is mouse-only; no stream presets/profiles; settings always start collapsed |
| 8 | Aesthetic and Minimalist Design | **3** | Safelight applied with good restraint; telemetry HUD is distinctive; but 6 font sizes break the typographic scale |
| 9 | Error Recovery | **2** | Audio capture failure falls back gracefully, but WebRTC disconnection is silent — room code vanishes with no user-facing message, no reconnection attempt |
| 10 | Help and Documentation | **1** | No tooltips on any control, no onboarding flow, no "?" help icon, no keyboard shortcut reference — a first-time presenter faces a dead empty state with zero guidance |
| **Total** | | **25/40** | **Acceptable** |

---

## Design Specificity Verdict

**LLM assessment:** The telemetry HUD overlay (gradient + monospace stats + safelight sparkline) is a genuinely authored element — it evokes a broadcast control-room feel that serves the streamer persona. However, the surrounding structure (card-based layout, shadcn/ui shells, conventional header-controls-footer arrangement) is category-interchangeable. Without the safelight accent and telemetry bar, this could be a VPN client, a download manager, or any dev-tools dashboard. The "darkroom" metaphor — confined safelight, layered darkness by density — doesn't read in the spatial composition.

**Deterministic scan:** The detector returned zero findings (exit code 0, empty array) — likely a false negative as the detector may not cover DESIGN.md-based token rules. Manual review found no inline style objects, but identified pervasive color drift: raw `text-gray-100` through `text-gray-600` used instead of semantic tokens, `bg-gray-900/80`/`bg-gray-800/50` instead of `bg-card`, `border-gray-800` instead of `border-border`. The most significant finding: the Desktop Audio picker selection uses Tailwind amber (`amber-950/40`, `amber-500/40`, `amber-200`) instead of safelight tokens — an undocumented second accent color. Border-radius inconsistency (4px vs. 8px), non-standard spacing (2px, 10px), and typography drift (9px/10px labels vs DESIGN.md's 12px spec, variable tracking) were also found.

---

## Overall Impression

The stream-in-progress experience is strong — the telemetry HUD is distinctive and the live mid-stream parameter updates are genuinely powerful. But the entire experience bookending that peak is weak: the cold-start empty state is discouraging, the disconnect/recovery path is silent, and the monolithic 2174-line component architecture blocks iterative improvement. The design system exists on paper (DESIGN.md) but diverges substantially in code — half the UI uses semantic tokens, half uses raw Tailwind utilities, and two undocumented accent colors (amber, emerald) dilute the safelight doctrine.

---

## What's Working

1. **Telemetry HUD overlay (StreamTelemetryBar):** The gradient overlay, monospace tabular-nums data cells, safelight "On Air" indicator, and bitrate sparkline create a broadcast-control-room aesthetic that serves the streamer persona directly. This is the most authored element in the UI and should be the benchmark for the rest of the interface.

2. **Four-layer audio auto-detection pipeline:** IPC name-match → renderer fallback → single-app heuristic → system audio fallback makes the tool feel intelligent rather than configuration-heavy. For the gamer persona, "it just works" is the gold standard.

3. **Live mid-stream encoder parameter updates without restart:** A genuine power-user feature. Changing FPS, bitrate, resolution, or audio source while broadcasting shows deep understanding of the streaming workflow.

---

## Priority Issues

**[P1] No connection-health or reconnection UI**
**Why it matters:** When WebRTC drops or degrades, the only signal is subtle amber/red text in the telemetry overlay. No toast, no status badge change, no reconnection spinner. For a streaming tool where the entire product value is reliable transmission, silent failure is trust-destroying.
**Fix:** Add a connection-quality indicator in the header (green/amber/red dot next to LIVE badge). Show a non-dismissible "Reconnecting…" banner with elapsed time when the LiveKit room disconnects. Surface ICE connection state changes as toasts.
**Suggested command:** `/impeccable harden apps/desktop/src/renderer/main.tsx`

**[P1] No keyboard shortcuts for any action**
**Why it matters:** The primary persona is a gamer — hands on keyboard, expects hotkeys for streaming operations. Every action requires reaching for the mouse. This is the single biggest friction point separating this from production-grade streaming tools.
**Fix:** Global keyboard shortcuts: Ctrl+Shift+S to toggle screenshare, Ctrl+Shift+C to copy room link, Ctrl+Shift+, to toggle settings. Register in Electron main process for global scope. Add a "?" shortcut overlay modal.
**Suggested command:** `/impeccable shape keyboard shortcuts`

**[P1] Monolithic 2174-line single-file component**
**Why it matters:** All state, effects, rendering, and sub-components live in one file. This blocks iteration, makes testing impossible, and guarantees merge conflicts.
**Fix:** Extract AudioAppPicker, SourcePicker, StreamSettingsPanel, VideoFileControls, RoomHeader, PreviewCard into dedicated component files. Extract hooks: useLiveKitRoom, useAudioCapture, useStreamTelemetry, useStreamSettings, useAudioAutoDetect. Per Task 9 in the backlog.
**Suggested command:** `/impeccable distill apps/desktop/src/renderer/main.tsx`

**[P2] Pervasive color-token drift from DESIGN.md**
**Why it matters:** Raw Tailwind gray utilities used instead of semantic tokens. Amber for Desktop Audio picker is not safelight and not a documented exception. Theme changes don't cascade — half the interface won't update.
**Fix:** Define explicit Tailwind theme extensions for every named color in DESIGN.md. Replace amber with safelight-glow/safelight tokens. Audit and replace all raw `gray-*`, `amber-*` usage.
**Suggested command:** `/impeccable audit apps/desktop/src/renderer/main.tsx`

**[P2] Empty state is underdesigned and discouraging**
**Why it matters:** A first-time presenter sees a giant 16:9 black rectangle with two lines of gray 12px text — no value proposition, no product character, no clear first step. It feels like a dead end, not an invitation.
**Fix:** Replace with an illustrated empty state: a stylized broadcast icon, one-line value prop, and a single prominent CTA. Remove the secondary text — let the CTA lead the user into the flow.
**Suggested command:** `/impeccable onboard apps/desktop/src/renderer/main.tsx`

**[P2] No stop screenshare confirmation dialog**
**Why it matters:** "Stop Screenshare" (destructive red) kills the stream instantly. In a live broadcast with active spectators, an accidental click is embarrassing.
**Fix:** Add a confirmation dialog: "Stop streaming? X spectators are watching." with "Cancel" and "Stop Streaming" buttons.
**Suggested command:** `/impeccable harden apps/desktop/src/renderer/main.tsx`

---

## Persona Red Flags

**Alex (Power User — Gamer/Streamer):**
- No keyboard shortcuts — every action requires mouse interaction
- No stream presets/profiles — switching configurations requires manual config each time
- No in-game overlay or always-on-top mini panel — must alt-tab out of fullscreen
- Stream settings panel always starts collapsed — must re-expand every session

**Sam (Accessibility-Dependent User):**
- Text sizes as small as 9px throughout violate WCAG SC 1.4.4
- Color-only state differentiation on copy button, selected audio states, degradation warnings
- No screen-reader announcements for audio capture failure, connection loss, or spectator join/leave
- No focus management when settings expand
- No prefers-reduced-motion override for LIVE pulse animation

**Gamer/Streamer (Primary Persona):**
- No stream delay indicator for competitive gamers concerned about stream sniping
- Audio auto-detection failure requires stopping and restarting screenshare mid-session
- No FPS counter overlay in the preview or dedicated game-capture performance panel

**Dev/Collaborator (Secondary Persona):**
- No participant management beyond a spectator count
- No chat or feedback channel during pair programming
- No recording capability for async review

---

## Minor Observations

- "Refresh" button on audio picker is redundant — audio apps auto-poll every 3s
- `streamSettingsOpen` ternary has identical `pb-0` branches (dead code)
- Casing inconsistency: "Screenshare" vs "screen share"
- "Create Live Room" button has no loading state — rapid clicks can trigger multiple requests
- "Video Controls" card breaks the 2-column grid
- Card component doesn't use DESIGN.md-specified backdrop-blur or surface-80% opacity
- API Endpoint input has no URL validation
- Audio picker max-h-56 means only ~4 apps visible before scrolling
- Sparkline/TelemetryCell types appear duplicated between StreamTelemetryBar.tsx and telemetry/ subdirectory

---

## Questions to Consider

- Should the DESIGN.md card spec (backdrop-blur, surface-80% bg) be applied, or is the current flat card style intentional?
- Is the lack of keyboard shortcuts a deliberate MVP scope decision or an oversight?
- Should the stream-settings collapsed/expanded state persist across app restarts?
- The product has no recording feature — intentionally out of scope, or should the UI reserve space for it?
