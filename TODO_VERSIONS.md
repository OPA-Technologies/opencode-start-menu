# TODO — Corrections comportement versions

> Document de travail. Révisé le 2026-10-20. Version courante : 0.50.6.
> Le chantier « versions » est livré pour ses phases 1 à 7, puis les volets
> date universelle / typologie / orchestration ; voir
> `ANALYSE_VERSIONS_RESUME.md` pour le contrat et l'état.

## Soldé

1. **`-V` : distinguer Debug et Release** — FAIT. `-V` lit la sortie du binaire
   (`<version>_<statut> (<date>)`) ; l'affichage bifilaire montre la base en
   complément quand elle diverge. `self_variant_label` n'existe plus.
2. **MAJ Release explicite dans `apply_edit`** — SANS OBJET par décision. La
   Release ne suit jamais le Debug et n'est jamais recompilée sans instruction
   (C2). `mirror_to_release` n'est volontairement pas introduit ; l'édition
   n'écrit que Debug + outils.
3. **AutoPatch Release : rattrapage** — FAIT (`executer_rebuild_release`
   déclare `last_compile_release_secs`).
4. **Documentation comportement** — FAIT (contrat C1–C5/B7 dans
   `ANALYSE_VERSIONS_RESUME.md`).
5. **Tests de régression** — FAIT :
   - édition via le menu → Debug mise à jour, Release inchangée
     (`l_edition_ecrit_les_constantes_de_la_source`,
     `l_edition_d_un_projet_sans_module_reste_en_base`) ;
   - commandes d'outils persistées (`l_edition_enregistre_les_commandes_d_outils`) ;
   - rebuild Release (`lancer_build_*`, rebuild déclaré à la garde) ;
   - diagnostic des 8 écarts (`coherence.rs`).
6. **Coûts et devises à zéro** — FAIT (commit 1bd77bd) : `cout_affiche()` rend
   « — » quand un coût nul accompagne des tokens, et le montant à zéro n'est
   gardé que pour une session sans tokens.
7. **Affichage bifilaire Release dans Réglages** — FAIT : le popup montre deux
   lignes binaires, Debug (base = valeurs éditées) et Release (base =
   valeurs persistées), comme le popup Détails.
8. **Distinguer « Release rattrapée » de « Release reconstruite »** — FAIT :
   `Ecart::variante()` + `libelle_maj_base()` nomment le rattrapage d'une
   Release ; le rebuild déclaré garde « Release reconstruite ».

## Encore ouvert

- **Rien de bloquant, rien de connu.**