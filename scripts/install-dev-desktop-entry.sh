#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"

install -Dm644 "$ROOT/apps/desktop/resources/slopcast.desktop" \
  "$DATA_HOME/applications/slopcast.desktop"
install -Dm644 "$ROOT/apps/desktop/resources/icon.png" \
  "$DATA_HOME/icons/hicolor/512x512/apps/slopcast.png"

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$DATA_HOME/applications" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -f -t "$DATA_HOME/icons/hicolor" >/dev/null 2>&1 || true
fi

echo "Installed dev desktop entry: $DATA_HOME/applications/slopcast.desktop"
echo "Restart the app (pnpm dev:desktop) to pick up the icon."
