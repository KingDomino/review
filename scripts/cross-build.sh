#!/usr/bin/env bash
# Build Linux release binaries (dist/review-aarch64, dist/review-x86_64) from macOS or Linux
# using Zig as the C toolchain. Requirements: rustup, cargo-zigbuild, and a Zig download:
#   cargo install cargo-zigbuild
#   ZIG_DIR=/path/to/zig-<host>-0.17.0 scripts/cross-build.sh
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
: "${ZIG_DIR:?set ZIG_DIR to an extracted Zig release}"
INC="$ZIG_DIR/lib/libc/include"
SHIM="$(mktemp -d)"
# MuPDF embeds fonts with GNU `ld -r -b binary`; emulate it so non-GNU hosts work too.
cp "$ROOT/scripts/ld-binary-shim.py" "$SHIM/ld" && chmod +x "$SHIM/ld"
export PATH="$SHIM:$ZIG_DIR:$HOME/.cargo/bin:$PATH" REVIEW_ZIG="$ZIG_DIR/zig" CARGO_ZIGBUILD_ZIG_PATH="$ZIG_DIR/zig" AR="$ZIG_DIR/zig ar"
mkdir -p "$ROOT/dist"
for arch in aarch64 x86_64; do
  hdr=$arch; [ "$arch" = x86_64 ] && hdr=x86
  rustup target add "$arch-unknown-linux-gnu" >/dev/null
  export REVIEW_ZIG_TARGET="$arch-linux-gnu.2.28"
  export BINDGEN_EXTRA_CLANG_ARGS="-isystem $INC/$hdr-linux-gnu -isystem $INC/generic-glibc -isystem $INC/$hdr-linux-any -isystem $INC/any-linux-any"
  (cd "$ROOT" && cargo zigbuild --release --locked --target "$arch-unknown-linux-gnu.2.28")
  cp "$ROOT/target/$arch-unknown-linux-gnu/release/review" "$ROOT/dist/review-$arch"
done
rm -rf "$SHIM"
ls -la "$ROOT/dist"
