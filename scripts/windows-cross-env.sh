#!/usr/bin/env bash

set -euo pipefail

XWIN_CACHE_DIR="${XWIN_CACHE_DIR:-$HOME/.cache/cargo-xwin}"
XWIN_ROOT="$XWIN_CACHE_DIR/xwin"
LLVM_BIN="$(dirname "$(command -v clang)")"

if [[ ! -d "$XWIN_ROOT/crt/lib" || ! -d "$XWIN_ROOT/sdk/include" ]]; then
    echo "error: xwin SDK cache not found at $XWIN_ROOT" >&2
    echo "  run: cargo xwin cache xwin" >&2
    return 1 2>/dev/null || exit 1
fi

export CC_x86_64_pc_windows_msvc="$LLVM_BIN/clang-cl"
export CXX_x86_64_pc_windows_msvc="$LLVM_BIN/clang-cl"
export AR_x86_64_pc_windows_msvc="$LLVM_BIN/llvm-lib"
export RANLIB_x86_64_pc_windows_msvc="$LLVM_BIN/llvm-lib"
export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER="$LLVM_BIN/lld-link"
export INCLUDE="$XWIN_ROOT/crt/include;$XWIN_ROOT/sdk/include/ucrt;$XWIN_ROOT/sdk/include/um;$XWIN_ROOT/sdk/include/shared;$XWIN_ROOT/sdk/include/winrt"
export LIB="$XWIN_ROOT/crt/lib/x86_64;$XWIN_ROOT/sdk/lib/um/x86_64;$XWIN_ROOT/sdk/lib/ucrt/x86_64"
export CL="-Wno-unused-command-line-argument -Wno-missing-field-initializers -Wno-undefined-var-template -Wno-unused-parameter -Wno-unused-function -Wno-extra-semi -Wno-inconsistent-missing-override -Wno-unused-private-field -Wno-return-type -Wno-c++11-narrowing -Wno-mismatched-tags -Wno-invalid-offsetof -Wno-format -Wno-parentheses-equality -Wno-tautological-compare -Wno-self-assign-overloaded -Wno-unused-but-set-variable -Wno-sign-compare -Wno-deprecated-declarations -Wno-error=missing-field-initializers"

echo "Windows cross-compile env loaded (clang-cl + lld-link + xwin cache)"
