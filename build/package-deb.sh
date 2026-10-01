#!/usr/bin/env bash
#
# Construit le paquet .deb installant op et opencode-wrap dans /usr/local/bin.
# Usage : build/package-deb.sh [arch]
#   arch : amd64 (défaut) | arm64
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="$(awk -F' +' '/^version *=/{print $3; exit}' "$ROOT/Cargo.toml" | tr -d '"')"
ARCH="${1:-amd64}"
BIN_NAME="op"

"$ROOT/build/build-release.sh" native

STAGE="$(mktemp -d)"
DEST="$STAGE/usr/local/bin"
mkdir -p "$DEST" "$STAGE/DEBIAN"
cp "$ROOT/target/release/opencode-start-menu" "$DEST/$BIN_NAME"
# Le wrapper est un crate séparé, construit par build-release.sh.
cp "$ROOT/wrapper/target/release/opencode-wrap" "$DEST/opencode-wrap"
chmod 755 "$DEST/$BIN_NAME" "$DEST/opencode-wrap"

SIZE_KB=$(( ($(stat -c %s "$DEST/$BIN_NAME") + $(stat -c %s "$DEST/opencode-wrap") + 1023) / 1024 ))

mkdir -p "$STAGE/usr/share/licenses/$BIN_NAME" \
         "$STAGE/usr/share/doc/$BIN_NAME"
cp "$ROOT/LICENSE" "$STAGE/usr/share/licenses/$BIN_NAME/LICENSE"
cp "$ROOT/debian/copyright" "$STAGE/usr/share/doc/$BIN_NAME/copyright"

cat > "$STAGE/DEBIAN/control" <<EOF
Package: opencode-start-menu
Version: $VERSION
Section: utils
Priority: optional
Architecture: $ARCH
Maintainer: Opa <opa@localhost>
Installed-Size: $SIZE_KB
License: GPL-3.0-or-later
Description: OpenCode Start Menu - launcher interactif de projets opencode
 Lanceur TUI des projets opencode : liste, recherche, lancement en session.
EOF

mkdir -p "$ROOT/dist"
DEB="$ROOT/dist/opencode-start-menu_${VERSION}_${ARCH}.deb"
dpkg-deb --build --root-owner-group "$STAGE" "$DEB" >/dev/null
rm -rf "$STAGE"

printf 'Paquet  : %s\n' "$DEB"