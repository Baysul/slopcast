# VAAPI H.264 Encoder Bitstream Bug — Stale POC Across IDR Boundaries

**Status: ROOT-CAUSED (2026-08-09), fix pending upstream / decision.**

**Symptom:** H.264 streams published by the desktop app's VAAPI encoder
ping-pong (visually alternate between two frames) — but **only when decoded
with hardware acceleration** in a web browser (Chromium, Firefox). Software
decoders (browser software decode, `ffmpeg`, `gstreamer`) all render the same
stream cleanly.

**Location:** vendored `webrtc-sys 0.3.41`
(`~/.cargo/registry/src/.../webrtc-sys-0.3.41/src/vaapi/vaapi_h264_encoder_wrapper.cpp`),
the VAAPI encoder shim the `livekit` crate (0.8.2) links. **Not** our capture /
delivery code (`packages/native-livekit/src/desktop_capture.rs`).

---

## The bug: `calc_poc` uses `static` state, never reset on IDR

`vaapi_h264_encoder_wrapper.cpp:1345`:

```cpp
static int calc_poc(VA264Context* context, int pic_order_cnt_lsb) {
  static int PicOrderCntMsb_ref = 0, pic_order_cnt_lsb_ref = 0;
  ...
  if (context->current_frame_type == FRAME_IDR)
    prevPicOrderCntMsb = prevPicOrderCntLsb = 0;   // reset for THIS call
  else {
    prevPicOrderCntMsb = PicOrderCntMsb_ref;        // stale from prev GOP!
    prevPicOrderCntLsb = pic_order_cnt_lsb_ref;
  }
  ...
  if (context->current_frame_type != FRAME_B) {
    PicOrderCntMsb_ref = PicOrderCntMsb;            // static never cleared
    pic_order_cnt_lsb_ref = pic_order_cnt_lsb;
  }
```

The POC reference state (`PicOrderCntMsb_ref`, `pic_order_cnt_lsb_ref`) is
**function-local `static`** — shared across encoder instances, survives
`Destroy()`/`Initialize()`, and is unsynchronized across threads (real
corruption in multi-stream / reuse / multi-thread scenarios).

`render_picture` (line 1387) feeds `calc_poc`'s output into
`CurrPic.TopFieldOrderCnt`, and `slice_header` (line 453) writes that value
straight into the bitstream's `pic_order_cnt_lsb`. So **the decoder receives
the corrupted POC**, not just the VAAPI driver.

### Trigger: the bug fires on **dropped keyframes**, not every IDR

A clean IDR **self-resets** the statics: in `calc_poc`, the IDR branch sets
`prevPicOrderCntMsb = prevPicOrderCntLsb = 0`, computes `TopFieldOrderCnt =
0`, then hits `if (current_frame_type != FRAME_B)` — true for IDR — and
overwrites `PicOrderCntMsb_ref = 0, pic_order_cnt_lsb_ref = 0` before
returning. So a normal IDR→P sequence is fine.

The jump happens when a keyframe is **requested but never encoded**:

```
h264_encoder_impl.cpp:205  is_keyframe_needed = key_frame_request && sending;
h264_encoder_impl.cpp:209  send_key_frame = is_keyframe_needed || frame_types[0]==kVideoFrameKey;
h264_encoder_impl.cpp:214  if (send_key_frame) key_frame_request = false;   // cleared BEFORE skip check
h264_encoder_impl.cpp:226  if (frame_types[0] == kEmptyFrame) return NO_OUTPUT;  // frame dropped
```

1. Keyframe requested (new spectator PLI, periodic `keyFrameInterval`, rate
   change) → `send_key_frame = true`, `key_frame_request` cleared.
2. The frame is dropped as `kEmptyFrame` (libwebrtc's frame-dropper skips
   exactly under encoder stalls, static-content resubmission, and load) →
   `Encode()` never runs → `calc_poc` never self-resets.
3. Next P-frame: `calc_poc(1)` against the stale statics (e.g.
   `pic_order_cnt_lsb_ref = 149`) → `PicOrderCntMsb = 0 + 256` →
   **`TopFieldOrderCnt = 257`** written into the bitstream, with no IDR
   reset in between.

This explains the **intermittent** "occasionally ping-ponging" symptom far
better than a deterministic per-GOP break: it needs a keyframe request to
coincide with a dropped frame. New spectators joining (PLI), encoder stalls,
and the keepalive resubmission path all increase the odds of that
coincidence.

### Why only hardware decoders break

- **VAAPI hardware decoders** trust the bitstream `pic_order_cnt_lsb` for DPB
  management and output ordering. A 256-frame jump at every keyframe
  mis-orders output → the display alternates between the IDR and the first P
  frame (the observed ping-pong).
- **Software decoders** (ffmpeg, gstreamer, Chromium/Firefox software decode)
  tolerate POC discontinuities in P-only streams (and often reorder by decode
  order for single-ref streams) → no visible issue.

This precisely matches the user's A/B test: same stream, hardware decode
broken, software decode (browser + ffmpeg + gstreamer) clean.

---

## Secondary bug: `level_idc` hardcoded to 4.1

`render_sequence` (line 1254) sets `context->seq_param.level_idc = 41`
(**Level 4.1** — the value encodes as `major×10 + minor`) unconditionally,
regardless of resolution. The factory advertises
`profile-level-id = 42e01f` (Constrained Baseline, Level 3.1 —
`vaapi_encoder_factory.cpp:22`), and the SPS the decoder receives is the
hand-rolled one via `build_packed_seq_buffer` → `sps_rbsp` → `level_idc`
(line 276). Note the source's own `/*SH_LEVEL_3*/` comment is wrong — it
mislabels the 4.1 value as Level 3.

H.264 levels constrain both frame size (`MaxFS`, MBs/frame) and throughput
(`MaxMBPS`, MBs/sec = MBs × fps). Against the hardcoded 4.1:

| Resolution | MBs | MBPS | Required | 4.1 valid? |
|---|---|---|---|---|
| 720p@30/60 | 3600 | 108k/216k | 3.1 / 3.2 | yes (over-declared) |
| 1080p@30 | 8160 | 244.8k | 4.0 | yes (over-declared) |
| 1080p@60 | 8160 | 489.6k | 4.2 | **no — under-declared (MBPS)** |
| 1440p@30 | 14400 | 432k | 5.0 | **no — under-declared (frame size)** |
| 2160p@30 | 32400 | 972k | 5.1 | **no — under-declared (frame size)** |

So the level bug is real but narrower than "invalid above 720p": it bites at
**1080p60** (the default `fps` preset) and any 1440p+ stream — the SPS
under-declares throughput/frame-size, which strict hardware decoders may
enforce. Software decoders ignore it. **Fix the level as well** (compute from
frame dimensions *and* framerate) when patching the POC bug.

---

## Fix plan (for upstream / a vendored fork)

Both fixes live in `vaapi_h264_encoder_wrapper.{h,cpp}` (+ `h264_encoder_impl.cpp`):

1. **Move the POC state into `VA264Context`** (per-instance, not `static`):
   add `int pic_order_cnt_msb_ref; int pic_order_cnt_lsb_ref;` to the struct
   (header lines 38-77), zero them in the constructor / `Initialize()`, and
   on `FRAME_IDR` in `calc_poc` set `prevPicOrderCntMsb = prevPicOrderCntLsb
   = 0` **and** reset the refs to 0. This removes the cross-instance sharing
   (a second encoder would otherwise inherit the first's POC state) and the
   cross-GOP leak.
2. **Don't clear `key_frame_request` until the IDR is actually encoded**:
   in `h264_encoder_impl.cpp`, move the `configuration_.key_frame_request =
   false` (line 214) to *after* the `kEmptyFrame` skip check (line 226-228).
   As written, the request is consumed even when the frame is dropped — so a
   dropped keyframe silently becomes a P-frame with stale POC state. Keeping
   the request pending forces the next non-dropped frame to be the IDR that
   self-resets `calc_poc`.
3. **Compute `level_idc` from resolution** in `render_sequence` instead of
   hardcoding `41`, using the H.264 Annex A table (min level whose
   `MaxFS >= (ceil(w/16) * ceil(h/16))` MBs **and** `MaxMBPS >= MBs × fps`).

To apply in this repo: fork `webrtc-sys 0.3.41` into e.g. `vendor/webrtc-sys`
and add to the workspace root `Cargo.toml` (precedent: the existing
`[patch.crates-io] tao` patch):

```toml
[patch.crates-io]
webrtc-sys = { path = "vendor/webrtc-sys" }
```

The fork must keep the crate's `build.rs` (compiles `src/vaapi/*.cpp` via cc
with libva) and the prebuilt `libwebrtc/` bundle intact.

---

## Evidence base (all verified 2026-08-09)

- `calc_poc` static state + IDR branch: `vaapi_h264_encoder_wrapper.cpp:1345-1376`
- IDR self-resets the statics (IDR ≠ B-frame → refs overwritten with 0): `:1370-1373`
- `forceIDR` resets `current_frame_*` but not POC refs: `:1786-1790`
- POC written to bitstream: `slice_header` `:452-453`
- Dropped-keyframe trigger: `h264_encoder_impl.cpp:204-229` (`key_frame_request`
  cleared at `:214` before `kEmptyFrame` skip at `:226`)
- `level_idc = 41` hardcoded: `render_sequence` `:1254`; SPS emission
  `sps_rbsp` `:258-276` via `build_packed_seq_buffer` `:559-571`
- Factory advertises `42e01f` (Constrained Baseline L3.1): `vaapi_encoder_factory.cpp:22`
- VAAPI encoder sync/ordering is otherwise correct: `Encode()` `:1780-1878`
  (`vaSyncSurface` before readback; no B-frames — `ip_period=1` from
  `h264_encoder_impl.cpp:150-153`)
- User A/B test (2026-08-09): hardware decode (Chromium/Firefox) ping-pongs;
  software decode (both browsers, `ffmpeg`, `gstreamer`) clean
- Prior delivery-loop fixes in this repo (buffer-instance ring, timestamp
  domain, stride-correct packing) addressed different, real issues but do not
  affect this bitstream bug
- POC nuance review (external, 2026-08-09): confirmed the static-architecture
  flaw (multi-stream/lifecycle/thread), corrected the trigger to
  dropped-keyframes — a clean IDR self-resets the statics, so the +256 jump
  requires a requested-but-skipped keyframe
