# Résumé Analyse Versions — opencode-start-menu

> Document de travail. Révisé le 2026-09-30, après la livraison de la
> valorisation du temps passé. Voir `TODO_VERSIONS.md` pour les actions
> encore ouvertes.

## État actuel

- **Cargo.toml** : `version = "0.20.0"` (ligne 3)
- **Registre** : `version_debug` = 0.20.0, `version_release` = 0.15.18
  (différentes, la release n'a pas été reconstruite)
- **Binaires** : Debug reconstruit, Release antérieur
- **`-V`** : affiche `CARGO_PKG_VERSION` suivi du statut, soit
  `0.20.0_Release`. Le suffixe est le **statut**, pas la variante : ce n'est
  pas une lecture de `version_release`.

## Écarts vs instructions utilisateur

| Règle | Implémentation | Statut |
|-------|----------------|--------|
| Modif menu → recompile + MAJ toutes refs | MAJ Debug seulement | ❌ |
| Modif code → auto-incrément z + MAJ même façon | Debug bump + Cargo.toml, Release attend rebuild | ⚠️ Partiel |
| Demande explicite MAJ → MAJ auto même façon | `apply_edit` fait Debug + historique, pas Release | ❌ |
| SAUF explicite : Release JAMAIS recompilé | Respecté (pas de build auto) | ✅ |
| Debug/Release infos DIFFÉRENTES tant que release pas rebûti | **OUI** dans le registre | ✅ |

## Cause de l'affichage trompeur

`op opencode-start-menu -V` renvoie la version **du manifeste** (0.20.0),
alors que le binaire release effectivement déployé est en 0.15.18. Les deux
valeurs coexistent legitimately dans le registre ; c'est l'affichage qui ne
les distingue pas.

À ne pas confondre avec l'ancien cas décrit dans les versions précédentes de
ce document : `self_variant_label()` (qui lisait le registre et ne montrait
que la variante Debug) a été **supprimé** — voir le commentaire
`main.rs:839`. Le problème est donc différent de celui analysé à l'origine,
et sa correction relève du TODO 1.

## Note sur l'AutoPatch

`source_hash` (SHA-256 de `Cargo.toml` + `src/**/*.rs`) sert à détecter les
modifications de code. Comme AutoPatch réécrit la ligne `version` de
`Cargo.toml`, le hash devait être resynchronisé **après** le bump, faute de
quoi chaque lancement détectait une fausse modification et incrémentait la
version à l'infini. Corrigé, avec test de régression
(`autopatch_ne_rebump_pas_a_l_chaque_lancement`).
