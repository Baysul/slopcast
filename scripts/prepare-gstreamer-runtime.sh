#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  exit 0
fi

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CACHE_HOME="${XDG_CACHE_HOME:-$HOME/.cache}"
BUILD_DIR="$CACHE_HOME/slopcast/gstreamer-runtime"
SOURCE_DIR="$BUILD_DIR/sources"
PLUGIN_DIR="$ROOT_DIR/apps/desktop/src-tauri/resources/gstreamer-plugins"
WEBRTC_VERSION="0.15.3"
RTP_VERSION="0.15.3"
WEBRTC_SHA256="cca3eb7568b17215505668b3cf09afc7780ecd03fe034efd02894da151abeac8"
RTP_SHA256="94a8636a5d8ab4d590e66c4cd103beac097d4c1f053f5723e47f141f5af67d0e"

mkdir -p "$BUILD_DIR" "$SOURCE_DIR" "$PLUGIN_DIR"
rm -f "$PLUGIN_DIR"/*.so "$PLUGIN_DIR/gst-plugin-scanner" "$PLUGIN_DIR/runtime-manifest.txt"

fetch_crate() {
  local name="$1"
  local version="$2"
  local checksum="$3"
  local archive="$BUILD_DIR/$name-$version.crate"
  local source="$SOURCE_DIR/$name-$version"

  if [[ ! -f "$archive" ]] || ! printf '%s  %s\n' "$checksum" "$archive" | sha256sum --check --status; then
    curl -fsSL -A cargo -o "$archive.tmp" \
      "https://static.crates.io/crates/$name/$name-$version.crate"
    mv "$archive.tmp" "$archive"
  fi
  printf '%s  %s\n' "$checksum" "$archive" | sha256sum --check --status

  if [[ ! -f "$source/Cargo.toml" ]]; then
    rm -rf "$source"
    tar -xzf "$archive" -C "$SOURCE_DIR"
  fi
}

fetch_crate gst-plugin-webrtc "$WEBRTC_VERSION" "$WEBRTC_SHA256"
fetch_crate gst-plugin-rtp "$RTP_VERSION" "$RTP_SHA256"

WEBRTC_TARGET="$BUILD_DIR/webrtc-target"
RTP_TARGET="$BUILD_DIR/rtp-target"
if [[ ! -f "$WEBRTC_TARGET/release/libgstrswebrtc.so" ]]; then
  CARGO_TARGET_DIR="$WEBRTC_TARGET" cargo build \
    --release --locked --features livekit \
    --manifest-path "$SOURCE_DIR/gst-plugin-webrtc-$WEBRTC_VERSION/Cargo.toml"
fi
if [[ ! -f "$RTP_TARGET/release/libgstrsrtp.so" ]]; then
  CARGO_TARGET_DIR="$RTP_TARGET" cargo build \
    --release --locked \
    --manifest-path "$SOURCE_DIR/gst-plugin-rtp-$RTP_VERSION/Cargo.toml"
fi

install -m 0755 "$WEBRTC_TARGET/release/libgstrswebrtc.so" "$PLUGIN_DIR/libgstrswebrtc.so"
install -m 0755 "$RTP_TARGET/release/libgstrsrtp.so" "$PLUGIN_DIR/libgstrsrtp.so"

# Elements that must exist on the host and be bundled. VA-API hardware
# encoders are handled separately below: the `va` plugin only registers its
# elements when a VA device (a /dev/dri/renderD* node) is present, so
# headless build hosts never expose them — and the publisher falls back to
# x264enc/x265enc at runtime when they are absent.
REQUIRED_ELEMENTS=(
  appsrc
  queue
  videoconvert
  videorate
  x264enc
  x265enc
  vp8enc
  vp9enc
  vp9parse
  av1parse
  h265parse
  av1enc
  h264parse
  audioconvert
  audioresample
  opusenc
  opusparse
  webrtcbin
  nicesrc
  nicesink
  dtlsenc
  srtpenc
  sctpenc
  sctpdec
  watchdog
  rtpbin
  rtpav1pay
  rtpvp8pay
  rtpvp9pay
  rtph264pay
  rtph265pay
)

# Bundled only when this host has a usable VA device; skipped otherwise so
# headless CI runners (no /dev/dri) can still prepare the runtime.
OPTIONAL_ELEMENTS=(
  vah264enc
  vah265enc
)

BUNDLED_ELEMENTS=("${REQUIRED_ELEMENTS[@]}")
for element in "${OPTIONAL_ELEMENTS[@]}"; do
  if gst-inspect-1.0 "$element" >/dev/null 2>&1; then
    BUNDLED_ELEMENTS+=("$element")
  else
    printf 'Skipping %s: no VA device on this host (software encoder fallback)\n' "$element" >&2
  fi
done

for element in "${BUNDLED_ELEMENTS[@]}"; do
  plugin_path=""
  while IFS= read -r line; do
    if [[ "$line" == *Filename* ]]; then
      plugin_path="${line##* }"
      break
    fi
  done < <(gst-inspect-1.0 "$element")
  if [[ ! -f "$plugin_path" ]]; then
    printf 'Unable to locate the GStreamer plugin supplying %s\n' "$element" >&2
    exit 1
  fi
  install -m 0755 "$plugin_path" "$PLUGIN_DIR/$(basename "$plugin_path")"
done

SCANNER_DIR=$(pkg-config --variable=pluginscannerdir gstreamer-1.0)
SCANNER_PATH="$SCANNER_DIR/gst-plugin-scanner"
if [[ ! -x "$SCANNER_PATH" ]]; then
  printf 'Unable to locate the GStreamer plugin scanner\n' >&2
  exit 1
fi
install -m 0755 "$SCANNER_PATH" "$PLUGIN_DIR/gst-plugin-scanner"

cat > "$PLUGIN_DIR/runtime-manifest.txt" <<EOF
stock gst-plugin-webrtc $WEBRTC_VERSION $WEBRTC_SHA256
stock gst-plugin-rtp $RTP_VERSION $RTP_SHA256
EOF

REGISTRY_FILE="$BUILD_DIR/isolated-registry.bin"
rm -f "$REGISTRY_FILE"
for element in livekitwebrtcsink rtpgccbwe "${BUNDLED_ELEMENTS[@]}"; do
  GST_PLUGIN_SYSTEM_PATH_1_0= \
    GST_PLUGIN_PATH_1_0="$PLUGIN_DIR" \
    GST_REGISTRY_1_0="$REGISTRY_FILE" \
    gst-inspect-1.0 "$element" >/dev/null
done

inspect_output=$(GST_PLUGIN_SYSTEM_PATH_1_0= \
  GST_PLUGIN_PATH_1_0="$PLUGIN_DIR" \
  GST_REGISTRY_1_0="$REGISTRY_FILE" \
  gst-inspect-1.0 livekitwebrtcsink)
if [[ "$inspect_output" != *"$WEBRTC_VERSION"* ]]; then
  printf 'Bundled livekitwebrtcsink is not stock version %s\n' "$WEBRTC_VERSION" >&2
  exit 1
fi

printf 'Prepared stock GStreamer LiveKit runtime in %s\n' "$PLUGIN_DIR"
