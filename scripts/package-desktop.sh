#!/usr/bin/env bash
# Package the native desktop without a GUI framework's bundler.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
bundles=""
target=""
build=1
while (($#)); do
  case "$1" in
    --bundles) bundles="$2"; shift 2 ;;
    --target) target="$2"; shift 2 ;;
    --no-build) build=0; shift ;;
    *) printf 'Unknown packaging argument: %s\n' "$1" >&2; exit 2 ;;
  esac
done
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' apps/desktop/Cargo.toml | head -1)
if [[ "$target" == *windows* || "${OS:-}" == Windows_NT ]]; then
  bundles="${bundles:-nsis}"
  [[ "$bundles" == nsis ]] || { echo 'Windows packaging supports --bundles nsis' >&2; exit 2; }
  if ((build)); then
    if [[ -n "$target" ]]; then cargo build --release --locked -p slopcast --target "$target"; else cargo build --release --locked -p slopcast; fi
  fi
  release="$ROOT/target/${target:+$target/}release"
  binary="$release/slopcast.exe"
  test -f "$binary"
  command -v makensis >/dev/null || { echo 'Install NSIS (makensis) to build the Windows installer.' >&2; exit 1; }
  mkdir -p "$release/bundle/nsis"
  python_command=python3
  if [[ "${OS:-}" == Windows_NT ]]; then python_command=python; fi
  "$python_command" scripts/package-windows.py "$binary" "$version" "$release/bundle/nsis/Slopcast-${version}-x64-setup.exe"
  exit 0
fi
[[ "$(uname -s)" == Linux ]] || { echo 'Desktop packaging supports Linux and Windows.' >&2; exit 1; }
[[ -z "$target" ]] || { echo 'Build Linux packages on a matching-architecture runner.' >&2; exit 1; }
bundles="${bundles:-deb,appimage,tar}"
case "$(uname -m)" in
  x86_64) arch=amd64; image_arch=x86_64 ;;
  aarch64) arch=arm64; image_arch=aarch64 ;;
  *) echo 'Unsupported Linux architecture' >&2; exit 1 ;;
esac
if ((build)); then
  bash scripts/prepare-gstreamer-runtime.sh
  cargo build --release --locked -p slopcast
fi
binary="$ROOT/target/release/slopcast"
plugins="$ROOT/apps/desktop/resources/gstreamer-plugins"
test -f "$binary"
for plugin in libgstrswebrtc.so libgstrsrtp.so libgstrtp.so gst-plugin-scanner; do
  test -f "$plugins/$plugin" || { printf 'Missing %s; run pnpm desktop:prepare-gstreamer first.\n' "$plugin" >&2; exit 1; }
done
bundle="$ROOT/target/release/bundle"
mkdir -p "$bundle"
staging=$(mktemp -d "$bundle/.stage.XXXXXX")
trap 'rm -rf -- "$staging"' EXIT
payload="$staging/payload"
app="$payload/usr/lib/slopcast"
mkdir -p "$app/licenses" "$app/resources/gstreamer-plugins" "$payload/usr/bin" "$payload/usr/share/applications" "$payload/usr/share/icons/hicolor/512x512/apps"
install -m755 "$binary" "$app/slopcast"
cp apps/desktop/resources/fonts/*-OFL.txt "$app/licenses/"
cp -a "$plugins/." "$app/resources/gstreamer-plugins/"
install -m644 apps/desktop/resources/slopcast.desktop "$payload/usr/share/applications/org.slopcast.Slopcast.desktop"
install -m644 apps/desktop/resources/icon.png "$payload/usr/share/icons/hicolor/512x512/apps/slopcast.png"
cat > "$payload/usr/bin/slopcast" <<'LAUNCH'
#!/bin/sh
set -eu
app=$(CDPATH= cd -- "$(dirname -- "$0")/../lib/slopcast" && pwd)
export LD_LIBRARY_PATH="$app/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$app/slopcast" "$@"
LAUNCH
chmod 755 "$payload/usr/bin/slopcast"
IFS=',' read -r -a requested <<< "$bundles"
for kind in "${requested[@]}"; do
  case "$kind" in
    deb)
      command -v dpkg-deb >/dev/null
      mkdir -p "$payload/DEBIAN" "$bundle/deb"
      cat > "$payload/DEBIAN/control" <<CONTROL
Package: slopcast
Version: $version
Architecture: $arch
Maintainer: Slopcast <parsley@duck.com>
Section: utils
Priority: optional
Depends: libvulkan1, libegl1, libwayland-client0, libxkbcommon0, libxkbcommon-x11-0, libxcb1, libxcb-xkb1, libx11-6, libfontconfig1, libfreetype6, libpipewire-0.3-0, libgstreamer1.0-0, libgstreamer-plugins-base1.0-0, gstreamer1.0-plugins-good, gstreamer1.0-plugins-bad, gstreamer1.0-plugins-ugly, gstreamer1.0-nice, xdg-desktop-portal
Description: Native desktop screen and application audio sharing
 Spectators watch from a browser without installing a client.
CONTROL
      dpkg-deb --root-owner-group --build "$payload" "$bundle/deb/slopcast_${version}_${arch}.deb"
      rm -r "$payload/DEBIAN"
      ;;
    tar)
      tar -czf "$bundle/Slopcast-${version}-linux-${arch}.tar.gz" -C "$payload" .
      ;;
    appimage)
      tool="${APPIMAGETOOL:-appimagetool}"
      command -v "$tool" >/dev/null || { echo 'Install appimagetool or set APPIMAGETOOL to its path.' >&2; exit 1; }
      appdir="$staging/Slopcast.AppDir"
      cp -a "$payload" "$appdir"
      mkdir -p "$appdir/usr/lib/slopcast/lib" "$bundle/appimage"
      # Include plugin dependencies too: these are loaded dynamically and absent from the executable's ldd output.
      python3 scripts/bundle-linux-libs.py "$appdir/usr/lib/slopcast"
      cp apps/desktop/resources/slopcast.desktop "$appdir/org.slopcast.Slopcast.desktop"
      cp apps/desktop/resources/icon.png "$appdir/slopcast.png"
      cp apps/desktop/resources/icon.png "$appdir/.DirIcon"
      cat > "$appdir/AppRun" <<'APPRUN'
#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
exec "$root/usr/bin/slopcast" "$@"
APPRUN
      chmod 755 "$appdir/AppRun"
      ARCH="$image_arch" APPIMAGE_EXTRACT_AND_RUN=1 "$tool" "$appdir" "$bundle/appimage/Slopcast-${version}-${image_arch}.AppImage"
      ;;
    *) printf 'Unsupported bundle: %s\n' "$kind" >&2; exit 2 ;;
  esac
done
