#!/usr/bin/env bash
#
# Construit le paquet .rpm installant op dans /usr/local/bin.
# Nécessite rpmbuild (paquet rpm-build).
# Usage : build/package-rpm.sh [arch]
#   arch : x86_64 (défaut) | aarch64
set -euo pipefail

if ! command -v rpmbuild >/dev/null; then
  printf 'rpmbuild introuvable (installer rpm-build).\n' >&2
  exit 1
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="$(awk -F' +' '/^version *=/{print $3; exit}' "$ROOT/Cargo.toml" | tr -d '"')"
ARCH="${1:-x86_64}"

"$ROOT/build/build-release.sh" native

RPMROOT="$(mktemp -d)"
mkdir -p "$RPMROOT/SOURCES" "$RPMROOT/BUILD" "$RPMROOT/BUILDROOT" "$RPMROOT/RPMS" "$RPMROOT/SPECS"
cp "$ROOT/target/release/opencode-start-menu" "$RPMROOT/SOURCES/op"
cp "$ROOT/LICENSE" "$RPMROOT/SOURCES/LICENSE"
sed "s/@VERSION@/$VERSION/; s/@ARCH@/$ARCH/" "$ROOT/build/rpm/op.spec.in" > "$RPMROOT/SPECS/op.spec"

mkdir -p "$ROOT/dist"
rpmbuild --define "_topdir $RPMROOT" -bb "$RPMROOT/SPECS/op.spec" >/dev/null
cp "$RPMROOT"/RPMS/*/*.rpm "$ROOT/dist/"
rm -rf "$RPMROOT"

printf 'Paquets :\n'
ls -1 "$ROOT"/dist/*.rpm