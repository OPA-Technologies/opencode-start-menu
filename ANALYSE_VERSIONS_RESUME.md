# Résumé Analyse Versions — opencode-start-menu

> Document de travail. Révisé le 2026-10-19, après la livraison des phases 1 à 7
> du chantier « versions ». Version courante : 0.50.6 (`Cargo.toml`). Voir
> `TODO_VERSIONS.md` pour les actions encore ouvertes.

## Principe directeur (cahier des charges)

La **vérité d'une variante est ce que son binaire déclare** via `-V`, au format
`<version>_<statut> (<date>)` (ex. `0.50.4_Stable (15-10-2026)`). Le registre
(`version_debug`/`status_debug`, `version_release`/`status_release`) n'est qu'un
**reflet** documenté ; la Release ne suit jamais le Debug toute seule.

- **C1** — Tout projet porteur de code déclare `RELEASE`, `STATUT`,
  `DATE_LAST_MAJ` et publie `<version>_<statut> (<date>)` en réponse à `-V`.
  Cette sortie fait foi.
- **C2** — Debug et Release sont **indépendantes**. La Release n'est jamais
  recompilée sans instruction explicite.
- **C3** — `registry::STATUSES` fixe la liste des statuts connus
  (Study, Alpha, Beta, Debug, Beta-Release, Release, Stable, Deprecated).
- **C4** — La conformité (`conformite::ecrire_constantes`) couvre tout projet
  porteur de code.
- **C5** — Les écarts sont exposés à chaque lancement et à chaque retour au
  menu ; les refus de correction sont conservés et re-notifiés.
- **B7** — `DATE_LAST_MAJ` est l'autorité au jour ; mtimes et empreintes SHA-256
  servent de départage fin.

## Affichage : bifilaire

Quand le binaire (sonde `-V`) ou la fiche de build diverge de la base, les deux
sont montrés : **binaire d'abord, base en complément**.
Ex. `0.50.4_Stable (15-10-2026) · base 0.50.5_Terminée`. Concerné : `op -V
<projet>`, `op -l` et les popups Détails / Réglages.

## Diagnostic : huit écarts (`src/coherence.rs`)

| Écart | Sens |
|-------|------|
| `SourcesAvantDebug` | le code a changé depuis le dernier build Debug |
| `DebugDivergeDeBase` | le binaire Debug diffère de la base |
| `ReleaseDivergeDeBase` | le binaire Release diffère de la base |
| `BaseDivergeDesSources` | la base ne suit pas la version des sources |
| `DebugDivergeDeRelease` | Debug et Release diffèrent (normal avant rebuild Release) |
| `FicheBuildPerimee` | la fiche de build ne décrit plus le binaire présent |
| `BinaireHorsContrat` | le binaire ne publie pas le contrat `-V` |
| `StatutHorsListe` | statut déclaré hors de la liste connue |

## Livraison par phases

1. Registre : statuts/versions par variante, dates déclarées, champs d'outils
   (migration non destructive des anciens registres).
2. Sonde par variante : auto (`target/<profil>`) + commande réglable.
3. `apply_edit` écrit la **source** (constantes du contrat), pas seulement la
   base ; la Release reste intacte.
4. `executer_maj_base` aligne les deux variantes ; les rebuilds sont déclarés à
   la garde (`date_*`, `last_compile_*_secs`, snapshots).
5. Affichages bifilaires (`libelle_bifilaire`, `fiche_pour_affichage`).
6. Diagnostic des 8 écarts (`coherence.rs`).
7. Commandes d'outils éditables (sonde et build par variante) et
   reconstruction automatique (`lancer_build`).

## Typologie (chantier 2)

Champ `typologie` du registre, fermé : `Code-App | Code-Lib | Doc-Lib |
Doc-office | Other`. Migré à la lecture par auto-détection (`src/` → `Code-*`,
sinon `Other`). Choix dans l'édition `p` (onglet Projet, champ 7), sur le
modèle du menu Statut. Il décide si op confie à opencode l'écriture des
constantes (`Code-*`) ou se contente du mtime de cible.

## Date universelle et cible (chantier 1)

- **Volet 3** — `DATE_LAST_MAJ` n'est réécrite que si le source change
  réellement : `date_conformisation` préserve la date si `RELEASE`/`STATUT`
  sont déjà alignés ; le rescan date depuis le mtime source ; `maj_date_cible`
  fait suivre la date de base au mtime de `target/<profil>` quand aucun `-V`
  n'y répond (le binaire fait toujours foi).
- **Volet 4** — 9ᵉ écart `DateCibleDesynchronisee` (action `MajBase`), exposé
  même sans manifeste : la cible `target/<profil>` est le seul fait.
- **`modified` et tri « Modifié »** alignés sur `target/debug` puis `release`
  puis l'ensemble du dossier (`registry::cible_modified_time`).

## Orchestration des constantes (chantier 2)

opencode-start-menu est chef d'orchestre : Rust est réécrit directement par le
menu, les autres langages sont délégués à opencode via une consigne ponctuelle
(`OPENCODE_INSTRUCTION`), portée par le plugin `projects-starter.ts` et
honorée aussi en Module/Bare. Le plugin déployé provient de
`config/plugins/projects-starter.ts` (via `build/setup-config.sh`).

## Ancien document (obsolète)

L'analyse d'origine portait sur `-V` renvoyant la version du manifeste et sur
`self_variant_label()` (supprimé). Ces points sont résolus : l'affichage
distingue désormais Debug et Release, et lit la vérité binaire.

`source_hash` (SHA-256 de `Cargo.toml` + `src/**/*.rs`) reste synchronisé après
chaque bump AutoPatch, faute de quoi chaque lancement détectait une fausse
modification et incrémentait la version sans fin.
