#!/usr/bin/env bash
#
# Assemble un projet OBS prêt pour `osc` dans build/obs/stage/ :
#   - source tarball avec répertoire racine opencode-start-menu-<version>/
#   - spec généré (version injectée)
# Usage : build/stage-obs.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="$(awk -F' +' '/^version *=/{print $3; exit}' "$ROOT/Cargo.toml" | tr -d '"')"

STAGE="$ROOT/build/obs/stage"
rm -rf "$STAGE"
mkdir -p "$STAGE"

tar -xzf "$ROOT/dist/op-$VERSION-src.tar.gz" -C "$STAGE" \
  --transform "s,^\./,opencode-start-menu-$VERSION/,"
tar -C "$STAGE" -czf "$STAGE/opencode-start-menu-$VERSION.tar.gz" \
  "opencode-start-menu-$VERSION"

sed "s/@VERSION@/$VERSION/" "$ROOT/build/obs/op.spec.in" > "$STAGE/op.spec"

printf 'Projet OBS prêt : %s\n' "$STAGE"