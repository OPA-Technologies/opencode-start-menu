# TODO — Corrections comportement versions

> Document de travail. Révisé le 2026-09-30. Version courante : 0.20.0.

## 1. `-V` : distinguer version du manifeste et version déployée (priorité haute)

`op <projet> -V` affiche aujourd'hui `CARGO_PKG_VERSION` + statut, soit
`0.20.0_Release`, alors que le binaire release déployé est en 0.15.18.

- Faire distinguer version Debug et version Release dans l'affichage
- Format à définir, par exemple `manifeste: 0.20.0 | déployé: 0.15.18`
- `self_variant_label` a été supprimé (commentaire `main.rs:839`) : ne pas
  chercher à le corriger, il n'existe plus

## 2. `apply_edit` : option MAJ Release explicite (priorité haute)

- Ajouter un paramètre/flag `mirror_to_release: bool` dans `apply_edit`
- Si vrai : mettre à jour `version_release` + `status_release` et
  `last_compile_release_secs = 0` pour forcer le rattrapage au prochain
  rebuild
- `mirror_to_release` est aujourd'hui totalement absent du code

## 3. ~~AutoPatch Release : rattrapage plus explicite~~ — FAIT

- ~~Mettre à jour `last_compile_release_secs` au rattrapage~~ : fait
- Reste possible : distinguer dans le message « Release rattrapée » de
  « Release rebuildée »

## 4. Documentation comportement (priorité moyenne)

- Documenter les sources de vérité : `Cargo.toml` vs registre vs binaire
- Clarifier que `op -V` renvoie la version du manifeste, alors que
  `op <projet> -V` renvoie celle du binaire du projet

## 5. Tests de régression (priorité basse)

Existants : `bump_patch_increments_patch`,
`bump_manifest_version_updates_package_only`,
`autopatch_ne_rebump_pas_a_l_chaque_lancement`.

Manquants :

- édition via le menu → vérifier Debug mise à jour, Release inchangée
- rebuild Release → vérifier le rattrapage version/statut

## 6. Coûts et devises toujours à zéro (priorité haute)

Le menu affiche `0,00 $` / `0,00 €` : la table `session` de `opencode.db`
ne contient aucune ligne avec `cost > 0`.

Cause trouvée et corrigée dans `wrapper/` : l'écriture des colonnes
`context_pct`, `currency`, `exchange_rate` et `cost_local` échouait
silencieusement. La requête utilisait `ORDER BY … LIMIT` dans un `UPDATE`,
que le SQLite embarqué de rusqlite refuse
(`near "ORDER": syntax error`) — le `sqlite3` système l'accepte, d'où la
confusion. Remplacé par une sous-requête sur `id`, avec test de
régression.

À confirmer après correction : lancer une session réelle puis vérifier que
`SELECT COUNT(*) FROM session WHERE cost > 0` devient non nul.
