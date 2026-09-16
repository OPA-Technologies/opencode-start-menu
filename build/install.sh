#!/usr/bin/env bash
#
# Installe le binaire op sous PREFIX (défaut : /usr/local).
# Usage : sudo ./install.sh [PREFIX]
set -euo pipefail

PREFIX="${1:-/usr/local}"
HERE="$(cd "$(dirname "$0")" && pwd)"

install -Dm755 "$HERE/bin/op" "$PREFIX/bin/op"
printf 'Installé : %s\n' "$PREFIX/bin/op"