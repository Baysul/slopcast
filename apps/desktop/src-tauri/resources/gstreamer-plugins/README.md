# Bundled GStreamer runtime

`pnpm desktop:prepare-gstreamer` populates this directory with the stock
`gst-plugin-webrtc` 0.15.3 LiveKit plugin, stock `gst-plugin-rtp` 0.15.3
congestion-control plugin, the GStreamer plugin scanner, and every system
plugin required by the Linux publisher. The VA-API encoders (`vah264enc`,
`vah265enc`) are bundled only when the host has a `/dev/dri/renderD*` node —
headless build hosts skip them and the publisher falls back to the software
encoders at runtime. Generated binaries and the runtime manifest are ignored
by Git and packaged as Tauri resources.
