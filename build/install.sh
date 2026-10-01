#!/usr/bin/env bash
#
# Installe op et opencode-wrap sous PREFIX (défaut : /usr/local).
#
# Les deux sont installés : `op` appelle opencode-wrap, qui transmet stdin et
# enregistre contexte et coût de session. Sans lui, opencode se lance mais plus
# rien n'est enregistré.
#
# Usage : sudo ./install.sh [PREFIX]
set -euo pipefail

PREFIX="${1:-/usr/local}"
HERE="$(cd "$(dirname "$0")" && pwd)"

if [ ! -f "$HERE/bin/opencode-wrap" ]; then
  printf "Erreur : %s/bin/opencode-wrap est absent de l'archive.\n" "$HERE" >&2
  printf 'Installation interrompue pour ne pas laisser une installation incomplète.\n' >&2
  exit 1
fi

install -Dm755 "$HERE/bin/op" "$PREFIX/bin/op"
install -Dm755 "$HERE/bin/opencode-wrap" "$PREFIX/bin/opencode-wrap"
printf 'Installé : %s\n' "$PREFIX/bin/op"
printf 'Installé : %s\n' "$PREFIX/bin/opencode-wrap"