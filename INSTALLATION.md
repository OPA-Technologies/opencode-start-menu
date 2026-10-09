# Installation et configuration (préstable)

## Paquets pré-compilés (voie recommandée)

La doctrine de distribution est le paquet pré-compilé : le binaire embarque
ses dépendances Rust (notamment `op-tui`), aucune chaîne de compilation n'est
requise sur la machine cible.

- Debian/Ubuntu : `build/package-deb.sh [amd64|arm64]` produit
  `dist/opencode-start-menu_<version>_<arch>.deb`.
- RPM/Fedora : `build/package-rpm.sh` produit le paquet correspondant.
- OBS : `build/stage-obs.sh` assemble un projet OBS prêt pour `osc`
  (tarball vendorisé + spec ; un garde-fou vérifie que chaque crate des deux
  verrous — paquet racine et `wrapper/` — est bien présent dans le vendor).

Chaque paquet installe deux binaires dans `/usr/local/bin` :
- `op` : le menu lui-même ;
- `opencode-wrap` : wrapper qui lance opencode en enregistrant le contexte et
  le coût de session — sans lui, le lancement se fait mais plus rien n'est
  consigné.

## Dépendance `op-tui`

`op-tui` est une dépendance `path` (`op-tui -> ../Libs/op-tui`). En paquet
pré-compilé elle est liée dans le binaire, donc invisible à l'installation.

Compilation depuis les sources en local :

```bash
ln -s ../Libs/op-tui op-tui   # depuis la racine du dépôt
cargo build --release
```

## Machine neuve

1. Installer le paquet (ou compiler puis placer les binaires manuellement).
2. Déployer les règles opencode, source unique dans ce dépôt :

   ```bash
   ./build/setup-config.sh
   ```

   Ceci copie `AGENTS.md` (langue, norme d'accolade, gestion des projets,
   ressources) vers `~/.config/opencode/instructions.md`, déploie les skills de
   `config/skills/`, rapproche `opencode.jsonc` et met de côté l'ancienne
   `projects-instructions.md`. Chaque fichier existant est sauvegardé
   (`*.bak-gerbe-<date>`) avant remplacement ; le registre `projects.json`,
   vivant, n'est jamais touché par le script. `XDG_CONFIG_HOME` est respecté.

3. Lancer `op` une première fois : le registre
   `~/.config/opencode/projects.json` est créé vide, puis chaque dossier de
   `~/projets/` est découvert au rescan. Les préférences du menu — ordre de
   tri, colonnes visibles, racine des bibliothèques (`~/projets/Libs`) — ont
   des valeurs par défaut et se règlent dans l'onglet Paramètres.

## Règles : ne jamais éditer le fichier déployé

`~/.config/opencode/instructions.md` est un miroir de `AGENTS.md`. Toute
retouche manuscrite du miroir est écrasée au prochain `setup-config.sh` ;
toute retouche doit être portée dans `AGENTS.md` puis redéployée.

## Registre et préférences entre machines

Le registre (`projects.json`) et les préférences du menu sont des données
utilisateur, fusionnées par la garde « vérité du disque » (les métadonnées de
build ne se modifient que par un chemin documenté). Pour transplanter une
installation complète sur une autre machine : copier `~/.config/opencode/`
— ou au minimum `projects.json`, `opencode.jsonc` et le dossier `skills/`.