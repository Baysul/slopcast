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
# Session buses D-Bus-activate portals and AT-SPI that outlive dbus-run-session, and
# each inherits this runtime dir. KWin's environ is unreadable (it has file
# capabilities), so its shim records the PID instead.
kill_runtime_dir_processes() {
  local signal=$1 environ pid var
  local -a vars pids=()
  if [[ -f "$runtime_dir/kwin.pid" ]]; then
    pid="$(<"$runtime_dir/kwin.pid")"
    kill -0 "$pid" 2>/dev/null && pids+=("$pid")
  fi
  for environ in /proc/[0-9]*/environ; do
    pid="${environ#/proc/}"
    pid="${pid%/environ}"
    [[ "$pid" == "$$" ]] && continue
    mapfile -d '' vars 2>/dev/null < "$environ" || continue
    for var in "${vars[@]}"; do
      [[ "$var" == "XDG_RUNTIME_DIR=$runtime_dir" ]] && pids+=("$pid") && break
    done
  done
  (( ${#pids[@]} == 0 )) && return 1
  kill "-$signal" "${pids[@]}" 2>/dev/null || true
}
cleanup() {
  if [[ -n "$server_pid" ]] && kill -0 "$server_pid" 2>/dev/null; then
    kill -TERM "$server_pid"
    local status=0
    wait "$server_pid" || status=$?
    if (( status != 0 && status != 129 && status != 130 && status != 143 )); then
      printf 'kwin-mcp exited with status %s during cleanup.\n' "$status" >&2
    fi
  fi
  local attempt
  for attempt in 1 2 3 4 5; do
    kill_runtime_dir_processes TERM || break
    sleep 1
  done
  kill_runtime_dir_processes KILL || true
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
# kwin-mcp starts KWin without Xwayland. Slopcast's CEF window is X11-only, so give
# the session a rootless Xwayland managed by KWin, and record KWin's PID so the
# launcher can find that display.
mkdir "$runtime_dir/bin"
printf '#!/bin/sh\necho $$ > "$XDG_RUNTIME_DIR/kwin.pid"\nexec %q --xwayland "$@"\n' \
  "$(command -v kwin_wayland)" > "$runtime_dir/bin/kwin_wayland"
chmod +x "$runtime_dir/bin/kwin_wayland"
export PATH="$runtime_dir/bin:$PATH"
cd "$ROOT"
uvx --from kwin-mcp==0.11.0 kwin-mcp --screenshot-images <&0 &
server_pid=$!
status=0
wait "$server_pid" || status=$?
server_pid=""

exit "$status"
