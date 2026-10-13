#!/usr/bin/env bash
# Build installers from the binaries in dist/ (run scripts/cross-build.sh first), from macOS or Linux:
#   Windows: Review-<version>-x64.msi, Review-<version>-arm64.msi   needs msitools (wixl, msibuild, msiinfo)
#   Linux:   review_<version>_amd64.deb / _arm64.deb,
#            review-<version>-1.x86_64.rpm / .aarch64.rpm            needs nfpm
#   scripts/package.sh [windows|linux]
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WHAT="${1:-all}"
VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$ROOT/Cargo.toml" | head -1)"
DIST="$ROOT/dist"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
die() { echo "error: $*" >&2; exit 1; }

if [ "$WHAT" = all ] || [ "$WHAT" = windows ]; then
  command -v wixl >/dev/null || die "wixl not found (brew install msitools, or dnf install msitools)"
  # wixl's stock installer UI (WixUI_Minimal), with Review's artwork.
  EXT=""
  for d in "$(dirname "$(command -v wixl)")"/../share/wixl-*/ext /usr/share/wixl-*/ext; do
    [ -d "$d" ] && EXT="$d" && break
  done
  [ -n "$EXT" ] || die "wixl's ext directory not found"
  cp -R "$EXT" "$WORK/ext"
  cp "$ROOT/assets/windows/installer-dialog.bmp" "$WORK/ext/ui/bitmaps/dlgbmp.bmp"
  cp "$ROOT/assets/windows/installer-banner.bmp" "$WORK/ext/ui/bitmaps/bannrbmp.bmp"

  # The license page shows LICENSE as RTF.
  {
    printf '{\\rtf1\\ansi\\deff0{\\fonttbl{\\f0 Segoe UI;}}\\fs16\n'
    sed -e 's/\\/\\\\/g' -e 's/{/\\{/g' -e 's/}/\\}/g' -e 's/$/\\par/' "$ROOT/LICENSE"
    printf '}\n'
  } > "$WORK/License.rtf"

  # File types, from the same list install.ps1 registers.
  exts="$(grep '^\$Exts' "$ROOT/windows/install.ps1" | grep -o "'[^']*'" | tr -d "'")"
  [ -n "$exts" ] || die "no extensions found in windows/install.ps1"
  {
    echo '<Include>'
    for e in $exts; do
      echo "<RegistryValue Root=\"HKCU\" Key=\"Software\\Classes\\.$e\\OpenWithProgids\" Name=\"Review.Document\" Type=\"string\" Value=\"\" />"
      echo "<RegistryValue Root=\"HKCU\" Key=\"Software\\Classes\\Applications\\review.exe\\SupportedTypes\" Name=\".$e\" Type=\"string\" Value=\"\" />"
      echo "<RegistryValue Root=\"HKCU\" Key=\"Software\\Review\\Capabilities\\FileAssociations\" Name=\".$e\" Type=\"string\" Value=\"Review.Document\" />"
    done
    echo '</Include>'
  } > "$WORK/filetypes.wxi"

  for arch in x64 arm64; do
    exe="$DIST/review-windows-$arch.exe"
    [ -f "$exe" ] || die "$exe not found (run scripts/cross-build.sh windows)"
    out="$DIST/Review-$VERSION-$arch.msi"
    rm -f "$out"
    # wixl has no arm64 target: build a 64-bit package, then mark it Arm64. Everything is
    # per-user (HKCU, %LOCALAPPDATA%), so nothing else depends on the architecture.
    (cd "$WORK" && wixl -a x64 --ext ui --extdir "$WORK/ext" -I "$WORK" \
      -D Version="$VERSION" -D Exe="$exe" -D Icon="$ROOT/assets/windows/review.ico" \
      -o "$out" "$ROOT/windows/review.wxs")
    # Take over an install made with install.cmd (same folder): drop its uninstaller and its
    # Apps entry, which would otherwise remain next to this package's. wixl can't express these.
    msibuild "$out" -q "INSERT INTO \`RemoveFile\` (\`FileKey\`, \`Component_\`, \`FileName\`, \`DirProperty\`, \`InstallMode\`) VALUES ('OldInstallScript', 'App', 'install.ps1', 'INSTALLDIR', 1)" \
      -q "CREATE TABLE \`RemoveRegistry\` (\`RemoveRegistry\` CHAR(72) NOT NULL, \`Root\` SHORT NOT NULL, \`Key\` CHAR(255) NOT NULL LOCALIZABLE, \`Name\` CHAR(255) LOCALIZABLE, \`Component_\` CHAR(72) NOT NULL PRIMARY KEY \`RemoveRegistry\`)" \
      -q "INSERT INTO \`RemoveRegistry\` (\`RemoveRegistry\`, \`Root\`, \`Key\`, \`Name\`, \`Component_\`) VALUES ('OldScriptUninstall', 1, 'Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Review', '-', 'App')"
    if [ "$arch" = arm64 ]; then
      code="$(msiinfo suminfo "$out" | sed -n 's/^Revision number (UUID): //p')"
      msibuild "$out" -s "Review" "KingDomino" "Arm64;1033" "$code"
    fi
  done
fi

if [ "$WHAT" = all ] || [ "$WHAT" = linux ]; then
  command -v nfpm >/dev/null || die "nfpm not found (brew install nfpm, or see https://nfpm.goreleaser.com)"
  STAGE="$WORK/linux"
  mkdir -p "$STAGE/icons/scalable/apps"
  MIMES="$(grep -v '^\s*$' "$ROOT/data/mime-types.txt" | tr '\n' ';')"
  sed -e "s|@EXEC@|review|g" -e "s|@MIME@|$MIMES|" "$ROOT/data/review.desktop.in" > "$STAGE/review.desktop"
  sed -e "s|@VERSION@|$VERSION|" -e "s|@DATE@|$(date +%F)|" "$ROOT/data/review.metainfo.xml.in" \
    > "$STAGE/io.github.kingdomino.review.metainfo.xml"
  for s in 16 24 32 48 64 128 256 512; do
    mkdir -p "$STAGE/icons/${s}x${s}/apps"
    cp "$ROOT/assets/icons/review-$s.png" "$STAGE/icons/${s}x${s}/apps/review.png"
  done
  cp "$ROOT/assets/review.svg" "$STAGE/icons/scalable/apps/review.svg"
  cp "$ROOT/LICENSE" "$STAGE/LICENSE"
  for pair in x86_64:amd64 aarch64:arm64; do
    IFS=: read -r arch goarch <<< "$pair"
    bin="$DIST/review-$arch"
    [ -f "$bin" ] || die "$bin not found (run scripts/cross-build.sh linux)"
    cp "$bin" "$STAGE/review"
    for fmt in deb rpm; do
      (cd "$STAGE" && VERSION="$VERSION" ARCH="$goarch" \
        nfpm package --config "$ROOT/packaging/nfpm.yaml" --packager "$fmt" --target "$DIST/")
    done
  done
fi
ls -la "$DIST"
