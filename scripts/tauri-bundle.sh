#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Tauri's CEF AppImage bundler runs quick-sharun.sh from FabianLars/Anylinux-AppImages.
# That fork still downloads helper sources pkgforge-dev deleted on 2026-09-10, so pin
# them to the fork's base commit.
anylinux_lib="https://raw.githubusercontent.com/pkgforge-dev/Anylinux-AppImages/d1bb26882eded9576bb0760aeb848179bbf64e9b/useful-tools/lib"
export ANYLINUX_LIB_SOURCE="$anylinux_lib/anylinux.c"
export GTK_CLASS_FIX_SOURCE="$anylinux_lib/gtk-class-fix.c"
cd "$ROOT/apps/desktop"
if [[ "$OSTYPE" != linux* ]]; then
  exec cargo tauri build "$@"
fi

# quick-sharun starts a session bus with dbus-launch and runs the app on it to trace
# dlopened libraries. That bus D-Bus-activates portals and AT-SPI and is never stopped,
# so confine everything to a private runtime dir and kill whatever still uses it.
runtime_dir="$(mktemp -d /tmp/slopcast-bundle.XXXXXX)"
kill_runtime_dir_processes() {
  local signal=$1 environ pid var
  local -a vars pids=()
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

unset DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS AT_SPI_BUS_ADDRESS XAUTHORITY
export XDG_RUNTIME_DIR="$runtime_dir"
cargo tauri build "$@"
