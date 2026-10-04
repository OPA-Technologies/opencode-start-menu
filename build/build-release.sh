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

# `op-tui` est une dépendance `path` : sans elle dans l'archive, la
# reconstruction OBS échoue sur un chemin introuvable, et le premier symptôme
# est un `cargo vendor` qui coule sans rien dire de plus.
OP_TUI="$ROOT/op-tui"
if [ ! -f "$OP_TUI/Cargo.toml" ]; then
  printf 'Erreur : %s/Cargo.toml introuvable.\n' "$OP_TUI" >&2
  printf 'op-tui est une dependance path : il doit etre resolvable depuis la racine.\n' >&2
  exit 1
fi

SRC_STAGE="$DIST/src-op-$VERSION"
rm -rf "$SRC_STAGE"
mkdir -p "$SRC_STAGE"
# `op-tui` est exclu : il entre dereference par le tar ci-dessous. Le laisser
# passer ici reintroduirait un lien symbolique dans le staging, et le tar
# suivant ecirait alors a travers, dans le depot lui-meme.
tar --exclude=.git --exclude=target --exclude=dist --exclude=./op-tui \
  -C "$ROOT" -cf - . | tar -C "$SRC_STAGE" -xf -
# `-h` : le lien symbolique `op-tui` est dereference, pour que l'archive
# contienne le crate et non un lien qui nepointera nulle part une fois extrait.
tar -h --exclude=.git --exclude=target -C "$ROOT" -cf - op-tui \
  | tar -C "$SRC_STAGE" -xf -

SRC_TARBALL="$DIST/op-$VERSION-src.tar.gz"
tar -C "$SRC_STAGE" -czf "$SRC_TARBALL" .

# Garde-fou : une archive sans op-tui s'installerait, et la reconstruction
# echouerait plus tard, loin de sa cause.
# `grep -c` et non `grep -q` : `grep -q` sort a la premiere correspondance,
# `tar` recoit SIGPIPE, et `pipefail` transforme cette sortie en echec -- le
# garde-fou_declencherait a tort sur une archive correcte.
if [ "$(tar -tzf "$SRC_TARBALL" | grep -c "^\./op-tui/Cargo.toml$")" -lt 1 ]; then
  printf "Erreur : op-tui absent de l'archive de sources.\n" >&2
  exit 1
fi
rm -rf "$SRC_STAGE"

cd "$DIST"
sha256sum "$(basename "$TARBALL")" "$(basename "$SRC_TARBALL")" > SHA256SUMS
{
  printf 'Archive : %s\n' "$(basename "$TARBALL")"
  printf 'Source  : %s\n' "$(basename "$SRC_TARBALL")"
  printf 'SHA256  : %s\n' "$DIST/SHA256SUMS"
}
rm -rf "$STAGE"