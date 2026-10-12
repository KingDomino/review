#!/usr/bin/env bash
# Review installer for Linux (per-user; sudo is only used to install build tools with dnf/apt).
#
#   ./install.sh              install (uses the prebuilt binary in dist/ when it matches this machine)
#   ./install.sh --build      always build from source
#   ./install.sh --default    ...and make Review the default app for PDFs and images
#   ./install.sh --uninstall  remove Review (your settings in ~/.config/review are kept)
set -euo pipefail

SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
BIN_DIR="$HOME/.local/bin"
BIN="$BIN_DIR/review"
APPS="$DATA_HOME/applications"
DESKTOP="$APPS/review.desktop"
ICONS="$DATA_HOME/icons/hicolor"
MIN_RUST_MINOR=95

FORCE_BUILD=0
SET_DEFAULT=0
UNINSTALL=0
for arg in "$@"; do
  case "$arg" in
    --build) FORCE_BUILD=1 ;;
    --default) SET_DEFAULT=1 ;;
    --uninstall) UNINSTALL=1 ;;
    -h|--help) sed -n '2,8p' "$0"; exit 0 ;;
    *) echo "Unknown option: $arg" >&2; exit 2 ;;
  esac
done

say()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

refresh_desktop() {
  have update-desktop-database && update-desktop-database -q "$APPS" 2>/dev/null || true
  have gtk-update-icon-cache && gtk-update-icon-cache -q -t -f "$ICONS" 2>/dev/null || true
  have kbuildsycoca6 && kbuildsycoca6 >/dev/null 2>&1 || true
  have kbuildsycoca5 && kbuildsycoca5 >/dev/null 2>&1 || true
}

if [ "$UNINSTALL" = 1 ]; then
  say "Removing Review"
  rm -f "$BIN" "$DESKTOP"
  find "$ICONS" -name 'review.png' -o -name 'review.svg' 2>/dev/null | xargs -r rm -f
  refresh_desktop
  say "Done. Settings in ~/.config/review were kept."
  exit 0
fi

[ "$(uname -s)" = "Linux" ] || die "This installer is for Linux."

case "$(uname -m)" in
  x86_64|amd64) ARCH=x86_64 ;;
  aarch64|arm64) ARCH=aarch64 ;;
  *) ARCH="$(uname -m)" ;;
esac

# ---------------------------------------------------------------------------
# 1. Get a binary: prebuilt if it runs here, otherwise build from source.
BUILT=""
PREBUILT="$SRC/dist/review-$ARCH"
if [ "$FORCE_BUILD" = 0 ] && [ -x "$PREBUILT" ]; then
  if "$PREBUILT" --version >/dev/null 2>&1; then
    say "Using prebuilt binary for $ARCH ($("$PREBUILT" --version))"
    BUILT="$PREBUILT"
  else
    warn "The prebuilt binary doesn't run on this system; building from source instead."
  fi
fi

install_build_deps() {
  local missing=()
  have cc || have gcc || missing+=(gcc)
  have c++ || have g++ || missing+=(gcc-c++)
  have make || missing+=(make)
  have pkg-config || missing+=(pkgconf-pkg-config)
  # bindgen needs libclang
  if ! ls /usr/lib64/libclang.so* /usr/lib/libclang.so* /usr/lib/*/libclang*.so* /usr/lib64/llvm*/lib*/libclang.so* >/dev/null 2>&1; then
    missing+=(clang-devel)
  fi
  [ ${#missing[@]} -eq 0 ] && return 0
  if have dnf; then
    say "Installing build tools: ${missing[*]} (sudo will ask for your password)"
    sudo dnf install -y "${missing[@]}" binutils
  elif have apt-get; then
    local apt=()
    for p in "${missing[@]}"; do
      case "$p" in
        gcc-c++) apt+=(g++) ;;
        pkgconf-pkg-config) apt+=(pkg-config) ;;
        clang-devel) apt+=(libclang-dev clang) ;;
        *) apt+=("$p") ;;
      esac
    done
    say "Installing build tools: ${apt[*]}"
    sudo apt-get install -y "${apt[@]}"
  elif have pacman; then
    say "Installing build tools"
    sudo pacman -S --needed --noconfirm base-devel clang pkgconf
  else
    die "Please install a C/C++ compiler, make, pkg-config and libclang, then re-run."
  fi
}

find_cargo() {
  local c
  for c in cargo "$HOME/.cargo/bin/cargo"; do
    if command -v "$c" >/dev/null 2>&1 || [ -x "$c" ]; then
      local rc="${c%cargo}rustc"
      [ "$c" = cargo ] && rc=rustc
      local minor
      minor="$("$rc" --version 2>/dev/null | sed -E 's/^rustc 1\.([0-9]+).*/\1/')"
      if [ -n "$minor" ] && [ "$minor" -ge "$MIN_RUST_MINOR" ] 2>/dev/null; then
        echo "$c"
        return 0
      fi
    fi
  done
  return 1
}

if [ -z "$BUILT" ]; then
  install_build_deps
  CARGO="$(find_cargo || true)"
  if [ -z "$CARGO" ]; then
    say "Installing a current Rust toolchain with rustup (user-local, ~/.cargo)"
    have curl || die "curl is needed to install Rust."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path
    CARGO="$HOME/.cargo/bin/cargo"
    "$HOME/.cargo/bin/rustup" update stable >/dev/null 2>&1 || true
  fi
  say "Building Review (first build takes a few minutes: MuPDF is compiled from source)"
  # Build outside the source tree so it works from read-only or shared folders.
  export CARGO_TARGET_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/review-build"
  (cd "$SRC" && "$CARGO" build --release --locked)
  BUILT="$CARGO_TARGET_DIR/release/review"
  [ -x "$BUILT" ] || die "Build failed."
fi

# ---------------------------------------------------------------------------
# 2. Install binary, icons and desktop entry.
mkdir -p "$BIN_DIR" "$APPS"
install -m 755 "$BUILT" "$BIN"
for s in 48 64 128 256 512; do
  install -D -m 644 "$SRC/assets/icons/review-$s.png" "$ICONS/${s}x${s}/apps/review.png"
done
install -D -m 644 "$SRC/assets/review.svg" "$ICONS/scalable/apps/review.svg"
MIMES="$(grep -v '^\s*$' "$SRC/data/mime-types.txt" | tr '\n' ';')"
sed -e "s|@EXEC@|$BIN|g" -e "s|@MIME@|$MIMES|" "$SRC/data/review.desktop.in" > "$DESKTOP"
chmod 644 "$DESKTOP"
refresh_desktop
say "Installed $BIN and “Review” in your application menu"

if [ "$SET_DEFAULT" = 1 ]; then
  if have xdg-mime; then
    say "Making Review the default app for PDFs and images"
    while read -r m; do
      [ -n "$m" ] && xdg-mime default review.desktop "$m" || true
    done < "$SRC/data/mime-types.txt"
  else
    warn "xdg-mime not found; choose Review in Dolphin via “Open With → Other Application…”."
  fi
fi

# ---------------------------------------------------------------------------
# 3. Optional runtime helpers.
hint() { printf '    %-38s %s\n' "$1" "$2"; }
echo
say "Optional extras:"
if ! ls /usr/lib64/libheif.so.1 /usr/lib/libheif.so.1 /usr/lib/*/libheif.so.1 >/dev/null 2>&1; then
  hint "HEIC/AVIF photos" "sudo dnf install libheif   (+ libheif-freeworld from RPM Fusion for iPhone photos)"
else
  hint "HEIC/AVIF photos" "ok (iPhone HEVC photos may also need libheif-freeworld from RPM Fusion)"
fi
have lp || hint "Printing" "sudo dnf install cups-client"
have spectacle || have gnome-screenshot || have grim || hint "File ▸ Take Screenshot" "sudo dnf install spectacle"
have spd-say || hint "Edit ▸ Speech" "sudo dnf install speech-dispatcher-utils"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) warn "$BIN_DIR is not on your PATH; the app-menu entry works regardless." ;;
esac
echo
say "Done. Run:  review file.pdf photo.jpg   — or open files from Dolphin."
