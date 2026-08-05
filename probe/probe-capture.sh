#!/usr/bin/env bash
# Permanent regression gate for the libwebrtc `pw_*` dlopen-shim collision
# (SCREEN-CAPTURE-INHOUSE.md §7.3). Linux-only: the collision is a PipeWire
# link-time phenomenon, so there is nothing to pin on other platforms.
#
# The probe pair pins the two facts that must never drift:
#   probe_a (no `DesktopCapturer` reference) -> must exit 0 AND enumerate apps
#     (proves our code no longer pulls the hook in at the minimal-binary level)
#   probe_b (deliberate `DesktopCapturer` reference) -> must still SIGSEGV
#     (proves the app must never reintroduce the reference; a future edit that
#     does pulls the 14-byte shim and crashes at startup unless re-armed)
#
# The app-binary readelf report is informational: as of Phase E, libwebrtc's
# peer connection factory keeps the PipeWire video-capture module (and with it
# `pipewire_stubs.o`) linked, so `arm_pipewire_shims()` must stay and the
# binary is expected to carry the shim (verified deviation, §1). The report
# flips to "SHIM-FREE" automatically once upstream ships a build without the
# module, which is the signal to delete `arm_pipewire_shims()` (§7.1/§7.2).
set -euo pipefail

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  grep '^#' "$0" | sed 's/^# \{0,1\}//'
  exit 0
fi

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "probe:capture is Linux-only (the PipeWire shim collision cannot occur on $(uname -s)). Skipping."
  exit 0
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "== building probe binaries =="
cargo build -p pw-conflict-probe --bins

echo "== probe_a: must exit 0 and enumerate apps (no capturer reference) =="
set +e
probe_a_out="$(./target/debug/probe_a 2>&1)"
code_a=$?
set -e
printf '%s\n' "$probe_a_out"
if [[ $code_a -ne 0 ]]; then
  echo "FAIL: probe_a exited $code_a (expected 0)"
  exit 1
fi
if ! grep -q "PROBE A: OK" <<<"$probe_a_out"; then
  echo "FAIL: probe_a did not report PROBE A: OK"
  exit 1
fi

echo "== probe_b: must SIGSEGV (exit 139) with the capturer reference =="
set +e
# The SIGSEGV is intentional — don't leave a core dump behind on systems that
# write them into the cwd.
ulimit -c 0
./target/debug/probe_b >/dev/null 2>&1
code_b=$?
set -e
if [[ $code_b -ne 139 ]]; then
  echo "FAIL: probe_b exited $code_b (expected 139/SIGSEGV)"
  exit 1
fi
echo "OK: probe_b SIGSEGVs as expected (exit 139)"

echo "== app-binary readelf report (informational; block tracked in §1) =="
found_app=0
for binary in "$ROOT/target/debug/slopcast" "$ROOT/target/release/slopcast"; do
  if [[ ! -f "$binary" ]]; then
    continue
  fi
  found_app=1
  # `grep -c` (not `grep -q`): the symbol table is huge, so `-q` exits on the
  # first match, readelf gets SIGPIPE mid-stream and pipefail turns the whole
  # pipeline into a failure, silently misreporting the shim state.
  if ! readelf -S "$binary" 2>/dev/null | grep -q ".symtab"; then
    echo "  $binary: stripped (no .symtab) — shim state not inspectable; gate uses target/debug/slopcast (§7.2)"
  elif [[ "$(readelf -Ws "$binary" 2>/dev/null | grep -c "_ZL11pw_init_ptr" || true)" -gt 0 ]]; then
    echo "  $binary: SHIM PRESENT (expected — upstream livekit still links the PipeWire video-capture module; §7.2 pending)"
  else
    echo "  $binary: SHIM-FREE — delete arm_pipewire_shims() and tighten the gate (§7.1/§7.2)"
  fi
done
if [[ $found_app -eq 0 ]]; then
  echo "  (no compiled slopcast binary found — app-binary report skipped)"
fi

echo "PROBE GATE: PASS (probe pair pinned; app binary documented above)"
