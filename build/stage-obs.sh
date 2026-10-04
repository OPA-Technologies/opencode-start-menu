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
  # `--sync` est obligatoire : `wrapper/` est un crate separe avec son propre
  # Cargo.lock, et le vendor du seul paquet racine ne contient pas les versions
  # qu'il a verrouillees. Sans lui, le wrapper se construit dans l'arbre OBS
  # seulement si le cache local se trouve etre aligne, et l'echec -- un crate
  # `js-sys` verrouille a une version absente du vendor -- n'a rien a voir avec
  # le crate qu'on croyait compiler.
  cargo vendor vendor --locked --sync wrapper/Cargo.toml >/dev/null

  # Garde-fou : chaque crate d'un des deux verrous doit etre vendorise. Les
  # paquets locaux (op-tui, opencode-start-menu, opencode-wrap) sont des
  # dependances `path`, ils ne sont pas vendorises -- c'est normal.
  manquants=$(
    grep -h '^name = ' Cargo.lock wrapper/Cargo.lock \
      | sed 's/^name = "//; s/"$//' | sort -u \
      | grep -vx -e op-tui -e opencode-start-menu -e opencode-wrap \
      | while read -r crate; do
          [ -d "vendor/$crate" ] || printf '%s ' "$crate"
        done
  )
  if [ -n "$manquants" ]; then
    printf 'Erreur : crates absents du vendor : %s\n' "$manquants" >&2
    printf 'Le vendor ne couvre pas tous les verrous : la construction echouera.\n' >&2
    exit 1
  fi
)

tar -C "$STAGE" -czf "$STAGE/opencode-start-menu-$VERSION.tar.gz" \
  "opencode-start-menu-$VERSION"

sed "s/@VERSION@/$VERSION/" "$ROOT/build/obs/opencode-start-menu.spec.in" > "$STAGE/opencode-start-menu.spec"

printf 'Projet OBS prêt : %s\n' "$STAGE"