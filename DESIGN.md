---
name: Slopcast
description: Room-based screen and audio sharing ecosystem
colors:
  bg: oklch(0.1596 0.0203 265.6)
  surface: oklch(0.2101 0.0318 264.7)
  elevated: oklch(0.2781 0.0296 256.8)
  border: oklch(0.2781 0.0296 256.8)
  control: oklch(0.3729 0.0306 259.7)
  muted-text: oklch(0.7137 0.0192 261.3)
  body-text: oklch(0.8717 0.0093 258.3)
  heading-text: oklch(0.967 0.0029 264.5)
  caption-text: oklch(0.52 0.02 260)
  safelight: oklch(0.6594 0.1096 57.8)
  safelight-hover: oklch(0.6036 0.1044 57)
  safelight-glow: oklch(0.6594 0.1096 57.8 / 0.15)
  destructive: oklch(0.5858 0.222 17.6)
typography:
  display:
    fontFamily: "system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif"
    fontSize: "clamp(2.25rem, 6vw, 3.75rem)"
    fontWeight: 800
    lineHeight: 1.1
    letterSpacing: "-0.02em"
  heading:
    fontFamily: "system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif"
    fontSize: "clamp(1.25rem, 3vw, 1.75rem)"
    fontWeight: 700
    lineHeight: 1.2
    letterSpacing: "-0.01em"
  body:
    fontFamily: "system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', sans-serif"
    fontSize: "0.875rem"
    fontWeight: 400
    lineHeight: 1.6
  label:
    fontFamily: "system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif"
    fontSize: "0.75rem"
    fontWeight: 600
    lineHeight: 1
    letterSpacing: "0.05em"
    textTransform: "uppercase"
  mono:
    fontFamily: "ui-monospace, SFMono-Regular, 'SF Mono', Menlo, Consolas, monospace"
    fontSize: "0.875rem"
    fontWeight: 600
rounded:
  sm: "6px"
  md: "8px"
  lg: "12px"
  xl: "16px"
  full: "9999px"
spacing:
  xs: "4px"
  sm: "8px"
  md: "16px"
  lg: "24px"
  xl: "32px"
components:
  button-primary:
    backgroundColor: "{colors.safelight}"
    textColor: "{colors.bg}"
    rounded: "{rounded.md}"
    padding: "12px 24px"
    fontWeight: 600
  button-primary-hover:
    backgroundColor: "{colors.safelight-hover}"
  button-secondary:
    backgroundColor: "{colors.elevated}"
    textColor: "{colors.heading-text}"
    rounded: "{rounded.md}"
    padding: "12px 24px"
    border: "1px solid {colors.border}"
  button-outline:
    backgroundColor: "transparent"
    textColor: "{colors.body-text}"
    rounded: "{rounded.md}"
    padding: "12px 24px"
    border: "1px solid {colors.border}"
  input:
    backgroundColor: "rgba(31, 41, 55, 1)"
    textColor: "{colors.heading-text}"
    rounded: "{rounded.md}"
    padding: "10px 16px"
    border: "1px solid {colors.control}"
  select-trigger:
    backgroundColor: "{colors.elevated}"
    textColor: "{colors.heading-text}"
    rounded: "{rounded.md}"
    padding: "8px 12px"
    border: "1px solid {colors.control}"
  select-content:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.heading-text}"
    rounded: "{rounded.md}"
    border: "1px solid rgba(31, 41, 55, 0.6)"
  card:
    backgroundColor: "rgba(17, 24, 39, 0.8)"
    textColor: "{colors.body-text}"
    rounded: "{rounded.lg}"
    padding: "24px"
    border: "1px solid rgba(31, 41, 55, 0.8)"
  badge-live:
    backgroundColor: "{colors.safelight-glow}"
    textColor: "{colors.safelight}"
    rounded: "{rounded.full}"
    padding: "4px 10px"
    border: "1px solid rgba(196, 128, 74, 0.3)"
  badge-info:
    backgroundColor: "{colors.elevated}"
    textColor: "{colors.muted-text}"
    rounded: "{rounded.full}"
    padding: "4px 10px"
    border: "1px solid {colors.control}"
  badge-disconnected:
    backgroundColor: "rgba(225, 29, 27, 0.1)"
    textColor: "{colors.destructive}"
    rounded: "{rounded.full}"
    padding: "4px 10px"
    border: "1px solid rgba(225, 29, 27, 0.2)"
---

# Design System: Slopcast

## Overview

**Creative North Star: "The Darkroom Studio"**

Slopcast's visual language is the darkroom at work. The interface is a dim, focused space where the only things that matter are the broadcast and the controls that serve it. Deep near-black backgrounds absorb distraction; layered grays define surfaces by density, not by light. The single roasted accent — Safelight — is the darkroom's glow: it signals live transmission, active capture, and anything that demands the user's attention. The rest of the interface recedes into shadow, precise and present but never competing with the stream.

This is not a gaming aesthetic — it is a *studio* aesthetic. Every control earns its place through purpose, not decoration. The stream is the subject; the chrome is the enlarger.

**Key Characteristics:**
- Monochrome with one shot of roasted amber; 90%+ of any surface is neutral.
- Layered darkness distinguishes hierarchy: deeper bg for the canvas, lighter surfaces for controls.
- Edges are soft but never timid — rounded corners on containers, sharp on controls.
- Backdrop blur on overlays and headers creates atmospheric depth without breaking the dark field.
- Motion is reserved for signal, not ornament: a pulse for live, a fade for presence, a glow for capture.

### Known drift

Both application surfaces are consolidated on the canonical Safelight value `#C4804A` via their respective Tailwind `safelight.DEFAULT` tokens. No Safelight color drift remains. No second accent colors exist in the visual system.

Desktop cards use `bg-card/80 backdrop-blur-md` for atmospheric depth against the dark canvas. Web cards use `bg-card` (full opacity, no backdrop blur) — the spectator interface is a simpler surface; the glass treatment is the presenter's prerogative.

## Colors

The palette is a darkroom: total black, paper-white for the stream, and one roasted safelight for live signal.

### Primary (Safelight)
- **Safelight** (oklch(0.6594 0.1096 57.8) / #C4804A): The single accent. LIVE indicators, primary buttons, focus rings, active capture states. Used sparingly — its rarity is its power.
- **Safelight Hover** (oklch(0.6036 0.1044 57) / #B0703E): Button and interactive hover states.
- **Safelight Glow** (oklch(0.6594 0.1096 57.8 / 0.15)): Subtle amber wash for LIVE badge backgrounds and selected audio-picker rows.

### Destructive
- **Signal Red** (oklch(0.5858 0.222 17.6) / #E11D48): Errors, disconnection, destructive actions, stream failures.

### Neutral
- **Darkroom Black** (oklch(0.1596 0.0203 265.6) / #090D16): Page background. The canvas everything sits on.
- **Surface** (oklch(0.2101 0.0318 264.7) / #111827): Cards, containers, sidebars. One step off absolute black.
- **Elevated** (oklch(0.2781 0.0296 256.8) / #1F2937): Controls, inputs, buttons (secondary), borders. The lightest dark.
- **Control Fill** (oklch(0.3729 0.0306 259.7) / #374151): Hover fills (`bg-accent`), input borders (`border-input`), scrollbar thumbs, info-badge borders.
- **Border** (oklch(0.2781 0.0296 256.8) / #1F2937): Structural lines — cards, inputs, dividers.
- **Muted Text** (oklch(0.7137 0.0192 261.3) / #9CA3AF): Secondary information, labels, placeholders, metadata.
- **Body Text** (oklch(0.8717 0.0093 258.3) / #D1D5DB): Primary readable content.
- **Heading Text** (oklch(0.967 0.0029 264.5) / #F3F4F6): Titles, headings, emphasized information.
- **Caption Text** (oklch(0.52 0.02 260)): Telemetry sub-values, sparkline captions, secondary metadata at the very bottom of the visual hierarchy.

### Named Rules

**The Safelight Rule.** The roasted amber accent appears on at most 10% of any screen. It is reserved exclusively for live/active/capture states and the primary call to action. Overuse dilutes its signal power — if everything glows, nothing is live.

**The No-Background Rule.** Backgrounds never carry color. Every visible tint comes from surface elevation (bg → surface → elevated), never from a colored wash. The Safelight accent lives only on elements with semantic meaning, never on chrome.

## Typography

**Display/Body Font:** System UI Stack (Inter/system-ui/-apple-system)
**Mono Font:** System Mono Stack (SF Mono / ui-monospace)

**Character:** Invisible and precise. The system stack ensures native rendering fidelity across platforms while the interface stays out of the stream's way. Typography serves hierarchy through weight and size alone — no decorative faces compete with the content. Labels are tracked uppercase for scanability; room codes and stream telemetry metrics are monospace for distinction.

### Hierarchy
- **Display** (800, clamp(2.25rem–3.75rem), 1.1, -0.02em): Hero headlines on the web home page. Only the product name and primary value prop. Reserved; not currently rendered in the desktop app.
- **Heading** (700, clamp(1.25rem–1.75rem), 1.2, -0.01em): Section titles, panel headers, modal titles. In the desktop app, the product wordmark uses `text-xl` (1.25rem) with `tracking-tight` — the clamp's bottom value.
- **Body** (400, 0.875rem/14px, 1.6): All reading text, descriptions, participant info. Rendered as `text-sm` (0.875rem) with `leading-relaxed` (1.625). Ideal measure 65–75 characters; long body paragraphs are constrained by `max-w-xs`.
- **Label** (600, 0.75rem/12px, 1, 0.05em uppercase): Section labels, form labels, telemetry bar labels, status labels. Rendered as `text-xs font-semibold uppercase tracking-wider`.
- **Mono** (600, 0.875rem/14px): Telemetry values, room codes, technical identifiers. Rendered as `text-sm font-mono font-semibold tabular-nums`. Sub-values (fps target, bitrate suffix) use `text-xs` in `text-caption-text`.

## Layout

The layout follows a single-column spine for the spectator and a two-column grid for the presenter.

**Web Spectator:**
- Home page: centered single-column max-w-2xl content area with full-bleed dark background.
- Room page: video owns the viewport (`fullBleed` aspect-video). Chrome floats as fixed overlays at the top (back / status / spectators / copy) and bottom (control bar). Sidebar participant panel deferred — spectator count is summarised in the header cluster instead.
- Max width: 80rem (1280px) for room, 32rem (512px) for home join card.
- Spacing rhythm safe areas: header overlay carries `pt-3 pb-8` so its bottom gradient fades before the stream; control bar carries `pt-12 pb-4` for the same reason on the underside.

**Desktop Presenter:**
- Single column with a max-w-5xl (1024px) content area.
- Sticky header (h-14) + status bar row, then a 16:9 preview card, then controls grid below.
- Controls arranged in a 2-column grid below the preview.
- Preview area is 16:9 aspect ratio with `rounded-xl` and `overflow-hidden`.

**Spacing rhythm:** 16px base unit. Sections separate by 24px (`space-y-8`); related items by 16px; tightly grouped items by 8px. More space above a heading than below it.

## Elevation & Depth

The system is flat by default — depth comes from tonal layering, not shadows. Surfaces stack from absolute black (bg) through surface gray to elevated gray, creating a physical metaphor of cards resting on a dark table.

- **Page bg** (oklch(0.1596 0.0203 265.6)): The table.
- **Cards/containers** (oklch(0.2101 0.0318 264.7) with border): One layer up. Desktop cards add `backdrop-blur-md` and 80% opacity for atmospheric depth against the canvas; web cards are full-opacity.
- **Controls/inputs** (oklch(0.2781 0.0296 256.8) with border): The interaction layer.
- **Overlays** (backdrop-blur-md): Atmospheric depth for modals, headers, banners, and video-overlay chrome — blurred translucency suggests glass or gelatin.

Shadows are sparse and reserved for emphasis: `shadow-2xl` on the desktop preview card. All cards carry `shadow-sm` by default. Hover states never lift with shadow — they shift border or background instead.

### Named Rules
**The Flat-By-Default Rule.** Surfaces are flat at rest. Shadow appears only as a response to elevation (preview card), never as a resting decoration.

## Shapes

- **Cards and containers:** 12px radius (`rounded-lg` on Card, `Card` component uses `rounded-lg`). Soft, approachable edges for grouped content.
- **Buttons:** 8px radius (`rounded-md` on the Button cva base). Feels tactile without being round.
- **Inputs and selects:** `rounded-md` on the shared `Input` and `Select` components — the `SelectTrigger` matches the `Input` shape exactly.
- **Badges and pills:** Full radius (`rounded-full`). Signals status, not interaction.
- **Video player:** 16px radius (`rounded-2xl` on the web player container) with `overflow-hidden`.
- **Audio picker rows / source thumbnails:** 8px radius (`rounded-lg`).

Borders are thin (1px), semi-transparent (0.5–0.8 opacity), and always the border gray — except the Safelight-amber border family on LIVE badges.

## Components

### Buttons
- **Shape:** 8px radius (`rounded-md`). Solid fill. No shadows. Font-size `text-sm`, weight `font-medium`.
- **Primary (Safelight):** `bg-primary text-primary-foreground` → roasted amber background, dark text. Hover: `bg-primary/90`.
- **Secondary:** `bg-secondary text-secondary-foreground` → elevated gray fill, light text. Hover: `bg-secondary/80`.
- **Outline:** `border border-input bg-background` → transparent, light text, 1px border. Hover: `bg-accent text-accent-foreground`.
- **Ghost:** Transparent, no border. Hover: `bg-accent text-accent-foreground`.
- **Destructive:** `bg-destructive text-destructive-foreground` → signal red, white text. Hover: `bg-destructive/90`.
- **Focus:** 2px ring (`focus-visible:ring-2 focus-visible:ring-ring`) with 2px offset. The ring color is the `--color-ring` token, which maps to Safelight. Visible focus is non-negotiable on icon-only buttons.

### Inputs
- **Shape:** `rounded-md`, h-10, px-3 py-2. Elevated fill (`bg-secondary`; the shadcn default is `bg-background` — desktop usage overrides to the Elevated interaction layer, see Elevation & Depth), 1px border (`border-input`). Placeholders in `text-muted-foreground`.
- **Focus:** Safelight ring (`focus-visible:ring-ring`) with offset.
- **Error:** Red border on error (`border-destructive`) with red helper text below.
- **No native `<select>` in the desktop app:** WebKitGTK renders it as a native GTK combobox that paints its own background over CSS `background-color` (Electron's Chromium did not), so it cannot be themed. All dropdowns use the shared shadcn `Select` (Radix) — see *Select (Desktop presenter)* below.
- **Mono variant:** Center-aligned, monospace, tracking-wide, for room-code entry.

### Select (Desktop presenter)
- **Structure:** shadcn `Select` (Radix, `@radix-ui/react-select`) in `components/ui/select.tsx` — `SelectTrigger` + `SelectValue` + `SelectContent` (portal) + `SelectItem`.
- **Trigger:** Identical shape to the shared `Input`: h-10, `rounded-md`, `border-input`, `bg-secondary`, px-3 py-2, text-sm, with the built-in `ChevronDown` indicator (muted, 50% opacity) and the Safelight `focus-visible` ring with offset.
- **Content (open menu):** `bg-popover` (Surface) with `text-popover-foreground`, `rounded-md`, a 1px translucent border (`border-border/60`), and `shadow-lg`. Opens with tw-animate-css fade/zoom/slide transitions; popper-aligns to the trigger width.
- **Items:** `SelectItem` rows, `rounded-sm`, highlighted in the Radix `focus:` state (`bg-accent text-accent-foreground`), with a left `Check` indicator on the selected item.
- **Value handling:** Radix values are strings; numeric state (fps, bitrate) is bridged with `String(value)` / `Number(v)` at the call site.
- **Why:** native `<select>` is avoided because WebKitGTK's GTK-combobox rendering ignores CSS backgrounds; a Radix listbox is fully CSS-driven and ships keyboard navigation and screen-reader semantics.

### Cards
- **Shape:** 12px radius (`rounded-lg`). Surface gray fill (`bg-card/80` on desktop, `bg-card` on web), 1px border, `backdrop-blur-md` on desktop only.
- **Internal padding:** 24px (`p-6`). Content section zeroes the top padding (`pt-0`) since the header already provides it.
- **Title:** `text-sm font-semibold leading-tight tracking-tight`. Actual usage universally overrides to `text-xs font-semibold uppercase tracking-wider text-muted-foreground` for card section labels.
- **No shadow by default on content sections.** The preview card carries `shadow-2xl` as deliberate emphasis.

### Badges
- **Shape:** Full radius pill (`rounded-full`). Padding `px-2.5 py-1`. Font `text-xs font-semibold uppercase tracking-wider`.
- **Live (Safelight):** `bg-safelight-glow text-safelight border-safelight/20`. Used for LIVE, Broadcasting, Active states. The live ping dot is `aria-hidden` decoration — the live region announces "Live" via the status pill's `role="status"` wrapper.
- **Info / Neutral:** `bg-secondary text-muted-foreground border-accent`. The canonical `Badge variant="info"` in `components/ui/Badge.tsx`.
- **Info on video overlay (RoomPage header):** When the same Connecting/Neutral status sits over live video, the Badge is overridden with translucent white (`bg-white/5 text-gray-400 border-white/10`) so the pill stays quiet against the moving pixels beneath. The override is local to the RoomPage header cluster and does not replace the canonical Badge variant.
- **Disconnected (Signal Red):** `bg-destructive/10 text-destructive border-destructive/20`. Used for Errors, Disconnected, Room Closed, Connection Lost.

### Navigation / Header
- **Web Header (HomePage):** Dark glass (`bg-background/80 border-b backdrop-blur-md`). Sticky top. Brand icon box + Slopcast wordmark (`text-base font-bold tracking-tight`).
- **Web Room Page header overlay:** Fixed top, gradient-faded to transparent (`bg-black/60`). The cluster holds `[Back] [Status Badge] [Spectators pill]` on the left and `[Copy link]` on the right. Long status text truncates with `max-w-[60vw] sm:max-w-[320px]`. Spectator pill is hidden below `sm` per the mobile-truncation rule.
- **Desktop Header:** Sticky `bg-background/80 backdrop-blur-md`. h-14 max-w-5xl. Left: wordmark (`text-xl font-bold leading-tight tracking-tight`) + LIVE badge (when sharing). Right: `Create Live Room` button, or `[Code + Copy Link]` cluster once a room exists.
- **Mobile:** Icon-based back navigation truncates secondary info. Spectator count pill hides below the `sm` breakpoint.

### Video Player
- **Container:** Aspect-video, black background, 16px radius (`rounded-2xl`), `overflow-hidden`. In `fullBleed` mode (RoomPage) it owns the viewport: `h-screen max-h-screen`, no radius, no border.
- **Idle state:** Center-aligned `Radio` icon + status message + optional Reconnect button.
- **Active state:** HTML5 video fills container. Top overlay (right-16) shows the AudioVisualizer pill — inset from the right edge so the always-on Copy button at top-right doesn't collide. Bottom overlay is a glass gradient control bar with play/pause, mute, volume slider (Safelight-accent range input `accent-[#C4804A]`), resync, and fullscreen toggle. Both overlays are hover-gated (`opacity-0 group-hover:opacity-100`).
- **Control buttons:** `bg-black/30 hover:bg-black/50 rounded-xl backdrop-blur-sm` — 12px radius, translucent black, glass blur. Focus ring: `focus-visible:ring-2 focus-visible:ring-safelight/70`.
- **Audio Visualizer:** Canvas-based frequency bars rendered in Safelight alpha ramp. 80×20px module in the top-right of the player; on fullBleed it sits at `top-4 right-16`.

### Stream Telemetry (Desktop presenter only)
- **Style:** Glass control bar fixed over the bottom of the preview (`from-black/95 via-black/75`). On-Air dot (`w-2 h-2 rounded-full bg-safelight motion-safe:animate-pulse`) + Safelight uppercase label (`text-xs font-semibold uppercase tracking-wider`), then mono tabular-nums cells for Codec / Resolution / Frame Rate (with target fps sublabel) / Bitrate / Packet Loss / Audio (codec + bitrate). Right rail: a 48s bitrate sparkline (Safelight `#C4804A`) and an Elapsed clock.
- **Cells:** Label uses the Label role (`text-xs font-semibold uppercase tracking-wider`). Value uses Mono role at 14px (`text-sm font-mono font-semibold tabular-nums`). Sub-values use `text-xs font-mono text-caption-text`. Degrade states flip label and value to the destructive token.
- **Sparkline caption:** `text-xs uppercase tracking-wider text-caption-text` — the quietest metadata tier.

### Window Audio Picker (Desktop presenter)
- **Shape:** Card section with label-style CardTitle + inline Refresh ghost button.
- **List items:** `rounded-lg` rows with hover state (`hover:border-input`). Unselected: `bg-background/60 border-border`. Selected: `bg-safelight-glow border-safelight/30 text-safelight`.
- **Auto-detected indicator:** Small Safelight pill. Signals that the native layer resolved the target application automatically.
- **Empty state:** Centered muted message.

## Do's and Don'ts

### Do:
- **Do** use the Safelight accent sparingly — it signals live, active, or actionable. If it's not one of those three, it should be neutral.
- **Do** let the stream own the viewport. The interface is the frame, not the picture.
- **Do** use border-gray-800 for structural separation; it's visible but not distracting.
- **Do** use backdrop blur for depth on overlays, headers, and banners — it creates atmospheric separation without lifting elements off the page.
- **Do** keep icon-only controls accessible: `aria-label`, `title`, and a visible `focus-visible` ring matching the Safelight ring spec.
- **Do** wrap dynamically-updating status pills in `role="status"` `aria-live="polite"` so screen readers announce transitions; mark their decorative ping dots `aria-hidden`.
- **Do** give error toasts `role="alert"` and decorative alert icons `aria-hidden="true"` so the message text is what the screen reader announces.
- **Do** use the Label role (`text-xs font-semibold uppercase tracking-wider`) for all form labels, card titles, and telemetry labels — keep the role consistent across surfaces.
- **Do** use `text-caption-text` for the quietest metadata tier (sparkline captions, telemetry sub-values).

### Don't:
- **Don't** add a second accent color. The darkroom has one roasted safelight. If you need to distinguish a secondary state, use neutral hierarchy (weight, size, opacity), not another color.
- **Don't** add shadows as decoration. Flat is the default; shadow is a deliberate response to elevation or focus.
- **Don't** use light mode or lighter background values for sections — the darkroom is dark everywhere. The darkest background is the canvas; every other surface is lighter, never lighter than Elevated for a fill.
- **Don't** use gradient backgrounds on content sections. The page background is flat black; surfaces are flat gray. The only gradient is the brand icon box, the Spectator Banner aspirational design (not currently shipped on Web), and the gradient-faded video overlay chrome.
- **Don't** hand-roll status pills and badges — use the shared `Badge` component. Compose variants via `variant` and use `className` only to make the container aware of its surface (e.g., the RoomPage video-overlay override).
- **Don't** inline icon-only buttons without an `aria-label` and visible `focus-visible` ring. Reviewer-at-a-glance convenience doesn't justify an inaccessible control.
- **Don't** use sub-12px text anywhere — 0.75rem (12px) is the floor. The Label role at `text-xs` is the smallest permitted size.
- **Don't** use hardcoded gray values (e.g., `text-gray-500`) in components — use semantic tokens (`text-muted-foreground`, `text-caption-text`, `text-foreground`) so the palette stays consistent across themes.
