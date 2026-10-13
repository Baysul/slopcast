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
for command in Xwayland xauth mcookie; do
  if ! command -v "$command" >/dev/null 2>&1; then
    printf 'Slopcast on virtual KWin requires %s.\n' "$command" >&2
    exit 1
  fi
done

auth_file="$(mktemp "$XDG_RUNTIME_DIR/slopcast-xauth.XXXXXX")"
xwayland_pid=""
cleanup() {
  if [[ -n "$xwayland_pid" ]] && kill -0 "$xwayland_pid" 2>/dev/null; then
    kill -TERM "$xwayland_pid"
    local status=0
    wait "$xwayland_pid" || status=$?
    if (( status != 0 && status != 143 )); then
      printf 'Xwayland exited with status %s during cleanup.\n' "$status" >&2
    fi
  fi
  rm -f -- "$auth_file"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

cookie="$(mcookie)"
xauth -f "$auth_file" add :0 . "$cookie"
# CEF requires X11; a rootful window needs no additional X11 window manager.
coproc XWAYLAND { exec Xwayland -auth "$auth_file" -displayfd 1 -nolisten tcp -geometry 1280x800; }
xwayland_pid=$XWAYLAND_PID
if ! read -r -t 15 display <&"${XWAYLAND[0]}" || [[ ! "$display" =~ ^[0-9]+$ ]]; then
  printf 'Xwayland did not report a ready display within 15 seconds.\n' >&2
  exit 1
fi
xauth -f "$auth_file" add ":$display" . "$cookie"
DISPLAY=":$display" XAUTHORITY="$auth_file" GDK_BACKEND=x11 QT_QPA_PLATFORM=xcb \
  "$ROOT/target/release/slopcast" "$@"
