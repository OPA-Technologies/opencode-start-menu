#!/usr/bin/env bash
#
# Construit le binaire en release et assemble l'archive tar.gz + checksums.
# Usage : build/build-release.sh [target]
#   target : "native" (défaut) ou un triplet rust, ex. x86_64-unknown-linux-gnu
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="$(awk -F' +' '/^version *=/{print $3; exit}' "$ROOT/Cargo.toml" | tr -d '"')"
BIN="opencode-start-menu"
TARGET="${1:-native}"

if [ "$TARGET" = "native" ]; then
  TARGET_SPEC=()
  BIN_PATH="$ROOT/target/release/$BIN"
else
  TARGET_SPEC=(--target "$TARGET")
  rustup target add "$TARGET" >/dev/null 2>&1 || true
  BIN_PATH="$ROOT/target/$TARGET/release/$BIN"
fi

cargo build --release "${TARGET_SPEC[@]}"

DIST="$ROOT/dist"
STAGE="$DIST/op-$VERSION"
rm -rf "$STAGE"
mkdir -p "$STAGE/bin"
if [ -f "$ROOT/LICENSE" ]; then
  mkdir -p "$STAGE/licenses"
  cp "$ROOT/LICENSE" "$STAGE/licenses/"
fi

cp "$BIN_PATH" "$STAGE/bin/op"
strip "$STAGE/bin/op" 2>/dev/null || true
cp "$ROOT/build/install.sh" "$STAGE/install.sh"

SUFIX="${TARGET//_/-}"
TARBALL="$DIST/op-$VERSION-$SUFIX.tar.gz"
tar -C "$DIST" -czf "$TARBALL" "op-$VERSION"

SRC_TARBALL="$DIST/op-$VERSION-src.tar.gz"
tar --exclude=.git --exclude=target --exclude=dist \
    -C "$ROOT" -czf "$SRC_TARBALL" .

cd "$DIST"
sha256sum "$(basename "$TARBALL")" "$(basename "$SRC_TARBALL")" > SHA256SUMS
{
  printf 'Archive : %s\n' "$(basename "$TARBALL")"
  printf 'Source  : %s\n' "$(basename "$SRC_TARBALL")"
  printf 'SHA256  : %s\n' "$DIST/SHA256SUMS"
}
rm -rf "$STAGE"