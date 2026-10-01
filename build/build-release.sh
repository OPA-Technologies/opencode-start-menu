#!/usr/bin/env bash
#
# Construit les deux binaires en release (menu + wrapper) et assemble
# l'archive tar.gz + checksums.
#
# Les deux sont indispensables : `op` appelle `/usr/local/bin/opencode-wrap`,
# qui transmet stdin et enregistre contexte et coût. Sans le wrapper dans
# l'archive, une installation propre lance opencode directement et n'enregistre
# plus rien.
#
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
  WRAP_PATH="$ROOT/wrapper/target/release/opencode-wrap"
else
  TARGET_SPEC=(--target "$TARGET")
  rustup target add "$TARGET" >/dev/null 2>&1 || true
  BIN_PATH="$ROOT/target/$TARGET/release/$BIN"
  WRAP_PATH="$ROOT/wrapper/target/$TARGET/release/opencode-wrap"
fi

cargo build --release "${TARGET_SPEC[@]}"
# Le wrapper est un crate séparé : il faut le construire explicitement.
(cd "$ROOT/wrapper" && cargo build --release "${TARGET_SPEC[@]}")

DIST="$ROOT/dist"
STAGE="$DIST/op-$VERSION"
rm -rf "$STAGE"
mkdir -p "$STAGE/bin"
if [ -f "$ROOT/LICENSE" ]; then
  mkdir -p "$STAGE/licenses"
  cp "$ROOT/LICENSE" "$STAGE/licenses/"
fi

cp "$BIN_PATH" "$STAGE/bin/op"
cp "$WRAP_PATH" "$STAGE/bin/opencode-wrap"
strip "$STAGE/bin/op" "$STAGE/bin/opencode-wrap" 2>/dev/null || true
cp "$ROOT/build/install.sh" "$STAGE/install.sh"

# Garde-fou : une archive sans wrapper s'installerait, mais la fonctionnalité
# d'enregistrement de session disparaîtrait sans le moindre avertissement.
if [ ! -f "$STAGE/bin/opencode-wrap" ]; then
  printf 'Erreur : opencode-wrap absent de l''archive.\n' >&2
  exit 1
fi

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