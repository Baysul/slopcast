#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  cat <<'HELP'
Builds and runs the PipeWire shim regression probes.

probe_a must enumerate audio apps without crashing.
probe_b must exit with SIGSEGV after referencing DesktopCapturer.
HELP
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
