#!/usr/bin/env bash
# Build release binaries for Linux (aarch64, x86_64) and Windows (x64, arm64) into dist/,
# from macOS or Linux, using Zig as the C toolchain.
# Requirements: rustup, cargo-zigbuild (cargo install cargo-zigbuild) and an extracted Zig release:
#   ZIG_DIR=/path/to/zig-<host>-0.17.0 scripts/cross-build.sh [linux|windows]
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
: "${ZIG_DIR:?set ZIG_DIR to an extracted Zig release}"
WHAT="${1:-all}"
INC="$ZIG_DIR/lib/libc/include"
SHIM="$(mktemp -d)"
trap 'rm -rf "$SHIM"' EXIT
# MuPDF embeds fonts with GNU `ld -r -b binary` on Linux; emulate it so non-GNU hosts work too.
cp "$ROOT/scripts/ld-binary-shim.py" "$SHIM/ld" && chmod +x "$SHIM/ld"
export PATH="$SHIM:$ZIG_DIR:$HOME/.cargo/bin:$PATH" REVIEW_ZIG="$ZIG_DIR/zig" CARGO_ZIGBUILD_ZIG_PATH="$ZIG_DIR/zig" AR="$ZIG_DIR/zig ar"
# bindgen should use the host clang's own intrinsic headers.
CLANG_INC="$(clang -print-resource-dir 2>/dev/null)/include"
mkdir -p "$ROOT/dist"

if [ "$WHAT" = all ] || [ "$WHAT" = linux ]; then
  for arch in aarch64 x86_64; do
    hdr=$arch; [ "$arch" = x86_64 ] && hdr=x86
    rustup target add "$arch-unknown-linux-gnu" >/dev/null
    export REVIEW_ZIG_TARGET="$arch-linux-gnu.2.28"
    export BINDGEN_EXTRA_CLANG_ARGS="-isystem $INC/$hdr-linux-gnu -isystem $INC/generic-glibc -isystem $INC/$hdr-linux-any -isystem $INC/any-linux-any"
    (cd "$ROOT" && cargo zigbuild --release --locked --target "$arch-unknown-linux-gnu.2.28")
    cp "$ROOT/target/$arch-unknown-linux-gnu/release/review" "$ROOT/dist/review-$arch"
  done
fi

if [ "$WHAT" = all ] || [ "$WHAT" = windows ]; then
  # Icon + version info, compiled once with Zig's resource compiler.
  (cd "$ROOT/assets/windows" && "$ZIG_DIR/zig" rc review.rc "$SHIM/review.res")
  export REVIEW_WINDOWS_RES="$SHIM/review.res"
  export CLANG_PATH="$(command -v clang)"
  for pair in "x64:x86_64-pc-windows-gnu:x86_64-pc-windows-gnu" "arm64:aarch64-pc-windows-gnullvm:aarch64-pc-windows-gnu"; do
    IFS=: read -r name target clangtarget <<< "$pair"
    rustup target add "$target" >/dev/null
    export BINDGEN_EXTRA_CLANG_ARGS="--target=$clangtarget -isystem $CLANG_INC -isystem $INC/any-windows-any"
    (cd "$ROOT" && cargo zigbuild --release --locked --target "$target")
    cp "$ROOT/target/$target/release/review.exe" "$ROOT/dist/review-windows-$name.exe"
    # Portable zip: review.exe + installer.
    pkg="$(mktemp -d)/Review"
    mkdir -p "$pkg"
    cp "$ROOT/target/$target/release/review.exe" "$pkg/review.exe"
    cp "$ROOT/windows/install.ps1" "$ROOT/windows/install.cmd" "$ROOT/windows/uninstall.cmd" "$ROOT/LICENSE" "$pkg/"
    (cd "$(dirname "$pkg")" && rm -f "$ROOT/dist/Review-windows-$name.zip" && zip -qr "$ROOT/dist/Review-windows-$name.zip" Review)
  done
fi
ls -la "$ROOT/dist"
