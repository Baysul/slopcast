#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
for command in uvx kwin_wayland dbus-run-session dbus-send dbus-update-activation-environment; do
  if ! command -v "$command" >/dev/null 2>&1; then
    printf 'kwin-mcp requires %s; see README.md (Native GUI automation).\n' "$command" >&2
    exit 1
  fi
done

runtime_dir="$(mktemp -d /tmp/slopcast-kwin-mcp.XXXXXX)"
server_pid=""
cleanup() {
  if [[ -n "$server_pid" ]] && kill -0 "$server_pid" 2>/dev/null; then
    kill -TERM "$server_pid"
    local status=0
    wait "$server_pid" || status=$?
    if (( status != 0 && status != 129 && status != 130 && status != 143 )); then
      printf 'kwin-mcp exited with status %s during cleanup.\n' "$status" >&2
    fi
  fi
  rm -rf -- "$runtime_dir"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

# Never let a live-session fallback reach the user's compositor or session bus.
unset DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS DBUS_STARTER_ADDRESS DBUS_STARTER_BUS_TYPE
unset AT_SPI_BUS_ADDRESS SESSION_MANAGER XAUTHORITY
export XDG_RUNTIME_DIR="$runtime_dir"
export XDG_SESSION_TYPE=wayland GDK_BACKEND=wayland QT_QPA_PLATFORM=wayland
cd "$ROOT"
uvx --from kwin-mcp==0.11.0 kwin-mcp --screenshot-images <&0 &
server_pid=$!
status=0
wait "$server_pid" || status=$?
server_pid=""

exit "$status"
