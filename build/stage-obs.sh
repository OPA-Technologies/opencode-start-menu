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

mkdir -p "$STAGE/opencode-start-menu-$VERSION/.cargo"
cat > "$STAGE/opencode-start-menu-$VERSION/.cargo/config.toml" <<EOF
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
EOF

(
  cd "$STAGE/opencode-start-menu-$VERSION"
  cargo vendor vendor --locked >/dev/null
)

tar -C "$STAGE" -czf "$STAGE/opencode-start-menu-$VERSION.tar.gz" \
  "opencode-start-menu-$VERSION"

sed "s/@VERSION@/$VERSION/" "$ROOT/build/obs/opencode-start-menu.spec.in" > "$STAGE/opencode-start-menu.spec"

printf 'Projet OBS prêt : %s\n' "$STAGE"