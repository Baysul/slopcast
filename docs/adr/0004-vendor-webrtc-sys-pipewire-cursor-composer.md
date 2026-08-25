# Vendor webrtc-sys and composite the PipeWire cursor in the shim

Status: accepted

m144's `BaseCapturerPipeWire` forwards `prefer_cursor_embedded` into `SharedScreenCastStream` (which then marks every frame `may_contain_cursor=true`) while `ScreenCastPortal` ignores the preference and still negotiates `cursor_mode=metadata` with the compositor. The frame claims to contain a cursor it does not contain, so `DesktopAndCursorComposer` skips its alpha-blend of the `SPA_META_Cursor` metadata and the presenter cursor never appears in the stream.

We vendor `webrtc-sys` (0.3.42) and patch its C++ shim: on the PipeWire arm, keep `include_cursor` true so the capturer is still wrapped in `DesktopAndCursorComposer` with `MouseCursorMonitorPipeWire`, but set `prefer_cursor_embedded` false so the composer actually blends the metadata KWin sends. Windows WGC paints the cursor into frames natively and is untouched. The shim also exposes process-wide cursor composition counters over cxx so the Rust side can warn the presenter when the compositor never delivers a usable cursor.

The alternative was a libwebrtc upgrade to the upstream fix (crrev 92db6816), which livekit's prebuilt does not yet ship, or moving composition into Rust frame delivery, which duplicates work the prebuilt already does. The vendor patch is small, pinned to the prebuilt's ABI (it reads the same `desktop_capture.ninja` defines), and removable once livekit ships the embedded-cursor fix.
