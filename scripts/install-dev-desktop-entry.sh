#!/usr/bin/env bash
# Install the Slopcast dev desktop entry + icon so Wayland compositors
# (KDE Plasma, GNOME) resolve the app icon for `pnpm dev:desktop` windows.
#
# Compositors match a window's app_id against a <app_id>.desktop file to
# find its icon; the dev binary's app_id is "slopcast" (GTK derives it from
# the executable name) and dev builds never install a desktop entry, so the
# taskbar would otherwise show the generic Wayland icon. This installs the
# entry once per machine; packaged builds get the same entry from the
# bundler-generated .deb (usr/share/applications/slopcast.desktop).
#
# Restart the app after installing — the icon is resolved at window map time.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"

install -Dm644 "$ROOT/apps/desktop/resources/slopcast.desktop" \
  "$DATA_HOME/applications/slopcast.desktop"
install -Dm644 "$ROOT/apps/desktop/resources/icon.png" \
  "$DATA_HOME/icons/hicolor/512x512/apps/slopcast.png"

# Refresh caches so the entry and icon resolve immediately.
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$DATA_HOME/applications" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -f -t "$DATA_HOME/icons/hicolor" >/dev/null 2>&1 || true
fi

echo "Installed dev desktop entry: $DATA_HOME/applications/slopcast.desktop"
echo "Restart the app (pnpm dev:desktop) to pick up the icon."
