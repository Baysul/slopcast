#!/usr/bin/env bash
# Environment for cross-compiling the Tauri/CEF desktop app to
# x86_64-pc-windows-msvc from Linux.  Pair with scripts/setup-windows-cross.sh,
# which installs clang-cl/lld-link, the official MSVC CRT + Windows SDK (xwin),
# llvm-rc and llvm-mt.
#
# Usage:
#   source scripts/windows-cross-env.sh
#   bash scripts/tauri-bundle.sh --target x86_64-pc-windows-msvc --bundles nsis

set -euo pipefail

XWIN_CACHE_DIR="${XWIN_CACHE_DIR:-$HOME/.cache/cargo-xwin}"
XWIN_ROOT="$XWIN_CACHE_DIR/xwin"
LLVM_BIN="$(dirname "$(command -v clang)")"

if [[ ! -d "$XWIN_ROOT/crt/lib" || ! -d "$XWIN_ROOT/sdk/include" ]]; then
    echo "error: xwin SDK cache not found at $XWIN_ROOT" >&2
    echo "  run: bash scripts/setup-windows-cross.sh" >&2
    return 1 2>/dev/null || exit 1
fi

export CC_x86_64_pc_windows_msvc="$LLVM_BIN/clang-cl"
export CXX_x86_64_pc_windows_msvc="$LLVM_BIN/clang-cl"
export AR_x86_64_pc_windows_msvc="$LLVM_BIN/llvm-lib"
export RANLIB_x86_64_pc_windows_msvc="$LLVM_BIN/llvm-lib"
export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER="$LLVM_BIN/lld-link"
export INCLUDE="$XWIN_ROOT/crt/include;$XWIN_ROOT/sdk/include/ucrt;$XWIN_ROOT/sdk/include/um;$XWIN_ROOT/sdk/include/shared;$XWIN_ROOT/sdk/include/winrt"
export LIB="$XWIN_ROOT/crt/lib/x86_64;$XWIN_ROOT/sdk/lib/um/x86_64;$XWIN_ROOT/sdk/lib/ucrt/x86_64"
# Two flags are load bearing, so they live in CL rather than in the Cargo config:
#
#   --target=x86_64-pc-windows-msvc
#     CEF's CMake build replaces CMAKE_CXX_FLAGS, so the target flag that Cargo
#     passes through CL_TARGET/CXXFLAGS is dropped for the wrapper objects.  They
#     then get compiled for the host ABI (COFF machine ARM64 on this runner) and
#     lld-link rejects them.
#   -Wno-macro-redefined
#     The wrapper compiles with -Werror and redefines CEF_API_VERSION.
export CL="--target=x86_64-pc-windows-msvc -Wno-macro-redefined -Wno-unused-command-line-argument -Wno-missing-field-initializers -Wno-undefined-var-template -Wno-unused-parameter -Wno-unused-function -Wno-extra-semi -Wno-inconsistent-missing-override -Wno-unused-private-field -Wno-return-type -Wno-c++11-narrowing -Wno-mismatched-tags -Wno-invalid-offsetof -Wno-format -Wno-parentheses-equality -Wno-tautological-compare -Wno-self-assign-overloaded -Wno-unused-but-set-variable -Wno-sign-compare -Wno-deprecated-declarations -Wno-error=missing-field-initializers"

echo "Windows cross-compile env loaded (clang-cl + lld-link + xwin cache)"
