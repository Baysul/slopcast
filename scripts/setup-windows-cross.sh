#!/usr/bin/env bash
# Provision a Windows x86_64 cross-compile toolchain on Linux.
#
# The desktop app is a Tauri v2 build against the CEF runtime, and its Windows
# installer (NSIS) is cross-compiled from Linux so every `build-desktop` entry
# can run on the same Ubuntu runner.  Compiling for `x86_64-pc-windows-msvc`
# needs considerably more than rustup's std:
#
#   * clang-cl, the MSVC-compatible clang driver.  CEF's C++ glue is built by the
#     cxx crate, whose MSVC support needs clang >= 19 (the current MSVC CRT
#     headers refuse older compilers).
#   * lld-link, the linker.
#   * the official MSVC CRT import libraries and Windows SDK headers and import
#     libraries, fetched by xwin under Microsoft's redistribution terms.
#   * llvm-rc / llvm-mt, the resource compiler and manifest tool; Windows'
#     rc.exe and mt.exe do not exist here.
#
# The script is idempotent.  On the Forgejo runner the job image already bakes
# all of this in, so every branch falls through and it only re-asserts paths.
set -euo pipefail

LLVM_VERSION="${LLVM_VERSION:-21}"
XWIN_CACHE_DIR="${XWIN_CACHE_DIR:-$HOME/.cache/cargo-xwin}"
export XWIN_CACHE_DIR

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }

# ---------------------------------------------------------------------------
# 1. clang / LLVM
# ---------------------------------------------------------------------------
need_llvm=0
if ! command -v clang >/dev/null 2>&1; then
    need_llvm=1
elif [[ "$(clang -dumpversion | cut -d. -f1)" -lt 19 ]]; then
    need_llvm=1
fi

if ((need_llvm)); then
    log "installing LLVM $LLVM_VERSION from apt.llvm.org"
    install -m 0755 -d /etc/apt/keyrings
    curl -fsSL https://apt.llvm.org/llvm-snapshot.gpg.key |
        sudo gpg --dearmor -o /etc/apt/keyrings/apt.llvm.org.gpg
    codename="$(. /etc/os-release && echo "$VERSION_CODENAME")"
    echo "deb [signed-by=/etc/apt/keyrings/apt.llvm.org.gpg] http://apt.llvm.org/${codename}/ llvm-toolchain-${codename}-${LLVM_VERSION} main" |
        sudo tee /etc/apt/sources.list.d/llvm-${LLVM_VERSION}.list >/dev/null
    sudo apt-get update
    sudo apt-get install -y --no-install-recommends "clang-${LLVM_VERSION}" "lld-${LLVM_VERSION}" "llvm-${LLVM_VERSION}"
fi

llvm_bin="$(dirname "$(command -v clang)")"
if [[ -x "/usr/lib/llvm-${LLVM_VERSION}/bin/clang-cl" ]]; then
    llvm_bin="/usr/lib/llvm-${LLVM_VERSION}/bin"
fi

# scripts/windows-cross-env.sh derives its tool directory from `command -v clang`
# and /usr/local/bin comes first in PATH, so expose every MSVC-driver tool there.
for tool in clang clang++ clang-cl lld-link llvm-lib llvm-rc llvm-mt llvm-ar llvm-readobj; do
    if [[ ! -x "$llvm_bin/$tool" ]]; then
        echo "error: $llvm_bin/$tool is missing; cannot cross-compile for Windows" >&2
        exit 1
    fi
    sudo ln -sf "$llvm_bin/$tool" "/usr/local/bin/$tool"
done

# ---------------------------------------------------------------------------
# 2. Pieces of the build that want Windows tools by their usual names
# ---------------------------------------------------------------------------
# CEF's CMake build looks up the resource compiler and manifest tool; how it
# spells them depends on the generator, so cover both spellings.
sudo ln -sf "$llvm_bin/llvm-rc" /usr/local/bin/rc
sudo ln -sf "$llvm_bin/llvm-mt" /usr/local/bin/mt

missing=()
command -v ninja >/dev/null 2>&1 || missing+=(ninja-build)
command -v makensis >/dev/null 2>&1 || missing+=(nsis)
if ((${#missing[@]})); then
    log "installing ${missing[*]}"
    sudo apt-get update
    sudo apt-get install -y --no-install-recommends "${missing[@]}"
fi

# ---------------------------------------------------------------------------
# 3. rustup std for the target
# ---------------------------------------------------------------------------
rustup target add x86_64-pc-windows-msvc

# ---------------------------------------------------------------------------
# 4. MSVC CRT + Windows SDK
# ---------------------------------------------------------------------------
if ! command -v xwin >/dev/null 2>&1; then
    log "installing xwin"
    cargo install --locked xwin
fi

if [[ ! -d "$XWIN_CACHE_DIR/xwin/crt/lib/x86_64" ]]; then
    log "downloading the Windows SDK and MSVC CRT into $XWIN_CACHE_DIR"
    XWIN_ACCEPT_LICENSE=true xwin --cache-dir "$XWIN_CACHE_DIR/crates" splat \
        --output "$XWIN_CACHE_DIR/xwin"
else
    log "reusing Windows SDK cache in $XWIN_CACHE_DIR"
fi

# The CEF wrapper includes <Softpub.h>.  The SDK ships softpub.h and both clang's
# preprocessor (via llvm-rc) and any case-sensitive walk of the include tree look
# for the exact MSVC spelling, so add the alias Windows gets for free.
sdk_um="$XWIN_CACHE_DIR/xwin/sdk/include/um"
if [[ -f "$sdk_um/softpub.h" && ! -e "$sdk_um/Softpub.h" ]]; then
    ln -sf "$sdk_um/softpub.h" "$sdk_um/Softpub.h"
fi

log "Windows cross toolchain ready: $(clang --version | head -1), SDK in $XWIN_CACHE_DIR"
