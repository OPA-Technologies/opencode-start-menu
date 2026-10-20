#!/usr/bin/env bash
#
# Déploie la configuration opencode depuis ce dépôt (source unique) vers le
# dossier de configuration de l'utilisateur.
#
# Copié :
#   - AGENTS.md              -> <config>/opencode/instructions.md
#   - config/skills/**       -> <config>/opencode/skills/**
# Retiré :
#   - la référence à projects-instructions.md dans opencode.jsonc, son contenu
#     étant fusionné dans AGENTS.md (le fichier est mis de côté).
#
# Chaque cible existante est sauvegardée en *.bak-gerbe-<date> avant
# remplacement. Le registre projects.json, fichier vivant du menu, n'est
# JAMAIS touché : seul le menu l'écrit.
#
# Prise en charge de XDG_CONFIG_HOME. Idempotent : relançable sans risque.
# Usage : ./build/setup-config.sh
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
CONF="${XDG_CONFIG_HOME:-$HOME/.config}"
OPENCODE_CONF="$CONF/opencode"

mkdir -p "$OPENCODE_CONF"

sauvegarde()
  {
    # Copie d'une cible existante vers *.bak-gerbe-<date>, sans jamais assumer
    # qu'une même cible soit sauvegardée deux fois au même horizon.
    local cible="$1"
    if [ -f "$cible" ]
      then
        local bak="$cible.bak-gerbe-$(date +%Y%m%d-%H%M%S)"
        if [ ! -f "$bak" ]
          then
            cp -a "$cible" "$bak"
            printf '  sauvegarde : %s\n' "$bak"
          fi
      fi
  }

printf 'Configuration opencode : %s\n' "$OPENCODE_CONF"

# --- Règles --------------------------------------------------------------
sauvegarde "$OPENCODE_CONF/instructions.md"
cp "$REPO/AGENTS.md" "$OPENCODE_CONF/instructions.md"
printf 'Règles déployées : AGENTS.md -> instructions.md\n'

# --- Ancien fichier de règles projet, fusionné dans AGENTS.md -------------
if [ -f "$OPENCODE_CONF/projects-instructions.md" ]
  then
    sauvegarde "$OPENCODE_CONF/projects-instructions.md"
    rm -f "$OPENCODE_CONF/projects-instructions.md"
    printf 'Anciennes règles projet retirées : projects-instructions.md\n'
  fi

# --- opencode.jsonc : les instructions relues au démarrage ----------------
if [ -f "$OPENCODE_CONF/opencode.jsonc" ]
  then
    if command -v python3 >/dev/null 2>&1
      then
        sauvegarde "$OPENCODE_CONF/opencode.jsonc"
        python3 - "$OPENCODE_CONF/opencode.jsonc" <<'PY'
import json
import sys

chemin = sys.argv[1]
with open(chemin, encoding="utf-8") as f:
    conf = json.load(f)

entrees = [e for e in conf.get("instructions", []) if "projects-instructions.md" not in e]
cible = "~/.config/opencode/instructions.md"
if cible not in entrees:
    entrees.insert(0, cible)
conf["instructions"] = entrees

with open(chemin, "w", encoding="utf-8") as f:
    json.dump(conf, f, ensure_ascii=False, indent=2)
    f.write("\n")
PY
        printf 'opencode.jsonc : liste des instructions rapprochée.\n'
      else
        printf 'Avertissement : python3 absent, opencode.jsonc non ajusté.\n' >&2
      fi
  fi

# --- Skills ---------------------------------------------------------------
if [ -d "$REPO/config/skills" ]
  then
    mkdir -p "$OPENCODE_CONF/skills"
    cp -a "$REPO/config/skills/." "$OPENCODE_CONF/skills/"
    find "$OPENCODE_CONF/skills" -name '*.sh' -exec chmod 755 {} +
    printf 'Skills déployés : config/skills -> skills\n'
  fi

# --- Plugins --------------------------------------------------------------
# Le plugin projects-starter amorce la session voulue par le menu (résumé de
# relance via OPENCODE_AUTO_RESUME, consigne ponctuelle via
# OPENCODE_INSTRUCTION) et affiche la liste des projets en lancement manuel.
if [ -d "$REPO/config/plugins" ]
  then
    mkdir -p "$OPENCODE_CONF/plugins"
    cp -a "$REPO/config/plugins/." "$OPENCODE_CONF/plugins/"
    printf 'Plugins déployés : config/plugins -> plugins\n'
  fi

printf 'Déploiement terminé. Les prochaines sessions opencode relisent les règles.\n'