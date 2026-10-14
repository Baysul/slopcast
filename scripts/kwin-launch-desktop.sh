#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
case "${XDG_RUNTIME_DIR:-}" in
  /tmp/slopcast-kwin-mcp.*) ;;
  *) printf 'Launch this helper through the project kwin-mcp server.\n' >&2; exit 1 ;;
esac
if [[ -z "${WAYLAND_DISPLAY:-}" || ! -x "$ROOT/target/release/slopcast" ]]; then
  printf 'Start an isolated KWin session and build Slopcast first.\n' >&2
  exit 1
fi
kwin_pid="$(cat "$XDG_RUNTIME_DIR/kwin.pid" 2>/dev/null || true)"
if [[ ! "$kwin_pid" =~ ^[0-9]+$ ]] || ! kill -0 "$kwin_pid" 2>/dev/null; then
  printf 'The session KWin is not running; restart it with session_start.\n' >&2
  exit 1
fi

# KWin starts Xwayland on the first connection to the X11 display it reserved. Display
# numbers are shared with the host, so find the one whose lock file holds KWin's PID.
display=""
for lock in /tmp/.X*-lock; do
  if [[ "$(tr -d ' \n' < "$lock")" == "$kwin_pid" ]]; then
    display="${lock#/tmp/.X}"
    display="${display%-lock}"
    break
  fi
done
if [[ -z "$display" ]]; then
  printf 'The session KWin has no Xwayland display; start it through scripts/kwin-mcp.sh.\n' >&2
  exit 1
fi

DISPLAY=":$display" GDK_BACKEND=x11 QT_QPA_PLATFORM=xcb exec "$ROOT/target/release/slopcast" "$@"
