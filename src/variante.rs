//! Contrat `-V` des projets et sonde unifiée.
//!
//! Chaque projet qui contient du code déclare, dans son module principal, trois
//! constantes figées à la compilation : `RELEASE` (`x.y.z`), `STATUT` (valeur de
//! la liste connue d'op) et `DATE_LAST_MAJ` (`jj-mm-yyyy`). Son `-V`/`--version`
//! publie alors une seule ligne, `<version>_<statut> (<jj-mm-yyyy>)`.
//!
//! Op lit cette ligne au lieu de déduire la version de ses propres métadonnées :
//! la vérité d'une variante est ce que son binaire déclare, pas ce que le
//! registre croit. Un binaire plus ancien, qui ne publie qu'une version nue,
//! reste lisible : statut et date sont alors vides.

use std::io::Read;
use std::path::Path;

/// État déclaré par le binaire d'une variante, lu depuis son `-V`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EtatVariante
  {
    /// Version semver (`x.y.z`).
    pub(crate) version: String,
    /// Statut déclaré (peut être vide pour un binaire antérieur au contrat).
    pub(crate) statut: String,
    /// Date de dernière mise à jour `jj-mm-yyyy` (vide hors contrat).
    pub(crate) date: String,
  }

/// État d'une variante lu depuis son `-V`, avec l'empreinte de sa sortie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LectureVariante
  {
    /// Triplé contractuel (version, statut, date).
    pub(crate) etat: EtatVariante,
    /// Empreinte SHA-256 de la ligne `-V` (signal fin de départage, B7).
    pub(crate) empreinte: String,
  }

/// Analyse la sortie de `-V` : format contractuel d'abord, version nue sinon.
pub(crate) fn parser_etat_variante(sortie: &str) -> Option<EtatVariante>
  {
    let ligne = sortie.lines().map(str::trim).find(|l| !l.is_empty())?;
    if let Some(etat) = parser_contractuel(ligne)
      {
        return Some(etat);
      }
    premiere_version_semver(ligne).map(|version| EtatVariante {
        version,
        statut: String::new(),
        date: String::new(),
      })
  }

/// Sonde une variante avec la commande personnalisée du projet, si réglée.
///
/// `sonde` vient des champs `probe_debug`/`probe_release` du registre : une
/// commande complète (`./bin/mreport -V`, `/usr/local/bin/foo --version`)
/// exécutée depuis la racine du projet. Vide ou absente : la cible auto
/// `target/<profil>/<binaire>` reprend la main.
///
/// Renvoie `None` si la cible n'existe pas, ne répond pas avant 2 s, échoue,
/// ou ne publie aucun état lisible.
pub(crate) fn lire_variante_avec_sonde(
  dir: &Path,
  profil: &str,
  sonde: Option<&str>,
  ) -> Option<LectureVariante>
  {
    let commande = sonde.map(str::trim).filter(|c| !c.is_empty());
    let sortie = match commande
      {
        Some(cmd) => interroger_commande(dir, cmd),
        None =>
          {
            let binaire = cible_auto(dir, profil)?;
            interroger_version(dir, &binaire)
          }
      }?;
    let etat = parser_etat_variante(&sortie)?;
    Some(LectureVariante {
        empreinte: empreinte_v(&sortie),
        etat,
      })
  }

/// Lit la vérité d'une variante depuis son binaire. `None` si introuvable
/// ou non contractuelle.
pub(crate) fn lire_variante(dir: &Path, profil: &str, sonde: Option<&str>) -> Option<LectureVariante>
  {
    lire_variante_avec_sonde(dir, profil, sonde)
  }

/// Source de vérité utilisée pour déterminer l'état d'une variante.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum SourceVerite
  {
    /// Lu directement depuis le binaire via `-V` à la lecture.
    Binaire,
    /// Provenant d'un snapshot de build validé.
    Snapshot,
    /// Provenant du manifeste/registre en l'absence de binaire contractuel.
    Manifeste,
    /// Dernier recours (mtime de cible) pour projets sans contrat.
    Mtime,
  }

/// Vérité d'une variante avec traçabilité de sa provenance.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct VeriteVariante
  {
    pub(crate) etat: EtatVariante,
    pub(crate) source: SourceVerite,
    /// Vrai si le cache/snapshot correspond toujours au binaire présent.
    pub(crate) frais: bool,
  }

/// Lit la vérité d'une variante.
///
/// - `prefer_cache=false` (CLI) : lit directement le binaire en priorité.
/// - `prefer_cache=true` (TUI/affichage répété) : utilise un cache uniquement
///   s'il est prouvé frais, sinon retombe sur la lecture du binaire.
pub(crate) fn verite_variante(
  dir: &Path,
  profil: &str,
  sonde: Option<&str>,
  prefer_cache: bool,
) -> Option<VeriteVariante>
  {
    if !prefer_cache
      {
        // Lecture directe : le réel prime toujours
        if let Some(lv) = lire_variante(dir, profil, sonde)
            && !lv.etat.version.is_empty()
          {
            return Some(VeriteVariante {
                etat: lv.etat,
                source: SourceVerite::Binaire,
                frais: true,
            });
          }
        return None;
      }
    // En mode cache préférentiel, on tente d'abord la lecture binaire
    // pour rester au plus près du réel quand le binaire a changé
    if let Some(lv) = lire_variante(dir, profil, sonde)
        && !lv.etat.version.is_empty()
      {
        return Some(VeriteVariante {
            etat: lv.etat,
            source: SourceVerite::Binaire,
            frais: true,
        });
      }
    None
  }

/// Cible auto d'une variante : binaire Cargo, sinon artefact non-Cargo.
///
/// Un projet sans manifeste (script, outil multi-langages) expose quand même
/// ses variantes sous `target/<profil>/`, sous le nom de son dossier : la
/// structure disque est unifiée pour tous les projets.
fn cible_auto(dir: &Path, profil: &str) -> Option<std::path::PathBuf>
  {
    let base = dir.join("target").join(profil);
    if let Some(nom) = nom_binaire(dir)
      {
        let chemin = base.join(nom);
        if chemin.is_file()
          {
            return Some(chemin);
          }
      }
    let nom = dir.file_name()?.to_string_lossy().to_string();
    if nom.is_empty()
      {
        return None;
      }
    let chemin = base.join(nom);
    chemin.is_file().then_some(chemin)
  }

/// Empreinte SHA-256 de la première ligne non vide de la sortie `-V`.
///
/// Signal fin de départage (§B7) : quand deux états portent la même date de
/// dernière mise à jour, l'empreinte distingue deux binaires dont la sortie
/// `-V` diffère, sans dépendre du mtime.
pub(crate) fn empreinte_v(sortie: &str) -> String
  {
    use sha2::{Digest, Sha256};
    let ligne = sortie.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let mut hasher = Sha256::new();
    hasher.update(ligne.as_bytes());
    format!("{:x}", hasher.finalize())
  }

/// Nom du binaire compilé d'un projet Rust, déduit de son manifeste.
///
/// Cargo nomme le binaire d'après le premier `[[bin]] name`, sinon le nom de
/// package — c'est ce que produit `cargo build` par défaut depuis `main.rs`.
fn nom_binaire(dir: &Path) -> Option<String>
  {
    let cargo = std::fs::read_to_string(dir.join("Cargo.toml")).ok()?;
    let mut dans_package = false;
    let mut dans_bin = false;
    let mut package_name: Option<String> = None;
    for ligne in cargo.lines()
      {
        let t = ligne.trim();
        if t.starts_with('[')
          {
            let section = t.trim_start_matches('[').trim_end_matches(']').trim();
            dans_package = section == "package";
            dans_bin = section == "bin";
            if !dans_bin
              {
                package_name = None;
              }
            continue;
          }
        if let Some(apres_egal) = t.split_once('=')
          {
            let cle = apres_egal.0.trim();
            if cle == "name"
              {
                let v = apres_egal.1.trim().trim_matches('"').to_string();
                if dans_bin
                  {
                    return Some(v);
                  }
                if dans_package
                  {
                    package_name = Some(v);
                  }
              }
          }
      }
    package_name
  }

/// Lance le binaire avec `-V` depuis la racine du projet, borne à 2 s, et
/// rend sa sortie (stdout **et** stderr : certains outils publient leur
/// état sur la sortie d'erreur — mreport, par exemple).
fn interroger_version(dir: &Path, binaire: &Path) -> Option<String>
  {
    let mut enfant = std::process::Command::new(binaire)
        .arg("-V")
        .current_dir(dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    attendre_enfant(&mut enfant)
  }

/// Exécue une commande de sonde personnalisée (`sh -c`) depuis la racine du
/// projet, bornée à 2 s, et rend stdout+stderr.
fn interroger_commande(dir: &Path, commande: &str) -> Option<String>
  {
    let mut enfant = std::process::Command::new("sh")
        .arg("-c")
        .arg(commande)
        .current_dir(dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    attendre_enfant(&mut enfant)
  }

/// Attend la fin d'un enfant de sonde (2 s max), puis lit ses deux flux.
fn attendre_enfant(enfant: &mut std::process::Child) -> Option<String>
  {
    let echeance = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop
      {
        if let Some(statut) = enfant.try_wait().ok()?
          {
            if !statut.success()
              {
                return None;
              }
            break;
          }
        if std::time::Instant::now() >= echeance
          {
            let _ = enfant.kill();
            let _ = enfant.wait();
            return None;
          }
        std::thread::sleep(std::time::Duration::from_millis(10));
      }
    let mut sortie = String::new();
    enfant.stdout.take()?.read_to_string(&mut sortie).ok()?;
    enfant.stderr.take()?.read_to_string(&mut sortie).ok()?;
    Some(sortie)
  }

/// Reconnaît `<version>_<statut> (<jj-mm-yyyy>)`.
fn parser_contractuel(ligne: &str) -> Option<EtatVariante>
  {
    let (avant, reste) = ligne.split_once('_')?;
    let version = avant.trim();
    if !est_semver(version)
      {
        return None;
      }
    let (statut_brut, date_brute) = reste.split_once('(')?;
    let statut = statut_brut.trim();
    let date = date_brute.trim().trim_end_matches(')').trim();
    if statut.is_empty() || !est_date(date)
      {
        return None;
      }
    Some(EtatVariante {
        version: version.to_string(),
        statut: statut.to_string(),
        date: date.to_string(),
      })
  }

/// `x.y.z` : trois groupes de chiffres non vides.
fn est_semver(texte: &str) -> bool
  {
    let parts: Vec<&str> = texte.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
  }

/// `jj-mm-yyyy` : dix caractères, chiffres aux bons rangs.
fn est_date(texte: &str) -> bool
  {
    let c: Vec<char> = texte.chars().collect();
    c.len() == 10
      && c.iter().enumerate().all(|(i, &x)| {
          if i == 2 || i == 5
            {
              x == '-'
            }
          else
            {
              x.is_ascii_digit()
            }
        })
  }

/// Première version semver (`x.y.z`) trouvée dans un texte.
fn premiere_version_semver(texte: &str) -> Option<String>
  {
    for token in texte.split_whitespace()
      {
        let t = token.trim_matches(|c: char| !c.is_ascii_digit() && c != '.');
        if est_semver(t)
          {
            return Some(t.to_string());
          }
      }
    None
  }

#[cfg(test)]
mod tests
  {
    use super::*;

    #[test]
    fn le_format_contractuel_est_lu_en_entier()
      {
        let etat = parser_etat_variante("0.50.2_Release (09-10-2026)\n").unwrap();
        assert_eq!(etat.version, "0.50.2");
        assert_eq!(etat.statut, "Release");
        assert_eq!(etat.date, "09-10-2026");
      }

    #[test]
    fn les_espaces_et_lignes_superflus_sont_toles()
      {
        let etat = parser_etat_variante("\n  0.9.9_Debug   (01-01-2026)  \n").unwrap();
        assert_eq!(etat.version, "0.9.9");
        assert_eq!(etat.statut, "Debug");
        assert_eq!(etat.date, "01-01-2026");
      }

    /// Un binaire antérieur au contrat ne publie qu'une version : elle reste
    /// lisible, sans statut ni date inventés.
    #[test]
    fn une_version_nue_reste_lisible()
      {
        let etat = parser_etat_variante("0.20.24\n").unwrap();
        assert_eq!(etat.version, "0.20.24");
        assert!(etat.statut.is_empty());
        assert!(etat.date.is_empty());
      }

    #[test]
    fn les_sorties_illisibles_ne_donnent_rien()
      {
        assert!(parser_etat_variante("").is_none());
        assert!(parser_etat_variante("   \n").is_none());
        assert!(parser_etat_variante("pas de version ici").is_none());
        assert!(
          parser_etat_variante("1.2_Release (01-01-2026)").is_none(),
          "version incomplète : pas le contrat, pas de version nue"
        );
      }

    #[test]
    fn une_date_mal_formee_retombe_sur_la_version_nue()
      {
        let etat = parser_etat_variante("1.2.3_Release (2026-01-01)").unwrap();
        assert_eq!(etat.version, "1.2.3", "la version nue reste trouvée");
        assert!(etat.statut.is_empty(), "pas de statut contractuel retenu");
        assert!(etat.date.is_empty());
      }

    #[test]
    fn l_empreinte_de_la_sortie_est_stable_et_discriminante()
      {
        let a = empreinte_v("0.50.3_Release (14-10-2026)\n");
        let b = empreinte_v("  0.50.3_Release (14-10-2026)  \n");
        assert_eq!(a, b, "les espaces superflus ne changent pas l'empreinte");
        assert_ne!(
          a,
          empreinte_v("0.50.3_Debug (14-10-2026)"),
          "un statut différent change l'empreinte"
        );
        assert_ne!(
          a,
          empreinte_v("0.50.3_Release (15-10-2026)"),
          "une date différente change l'empreinte"
        );
        assert_eq!(a.len(), 64, "empreinte SHA-256 en hexadécimal");
      }

    #[test]
    fn une_sortie_vide_a_une_empreinte_mais_aucun_etat()
      {
        // L'empreinte existe toujours (deterministe), mais l'état reste
        // introuvable : c'est `lire_variante` qui exige un état lisible.
        assert_eq!(empreinte_v(""), empreinte_v("   \n"));
        assert!(parser_etat_variante("").is_none());
      }

    /// Écrit un exécutable shell jouant le rôle du binaire d'une variante.
    fn ecrire_binaire(chemin: &Path, corps: &str)
      {
        std::fs::create_dir_all(chemin.parent().unwrap()).unwrap();
        std::fs::write(chemin, format!("#!/bin/sh\n{corps}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(chemin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
      }

    fn repertoire_temp(nom: &str) -> std::path::PathBuf
      {
        let dir = std::env::temp_dir().join(format!("op-variante-{nom}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
      }

    /// Projet sans Cargo.toml : l'artefact `target/debug/<nom du dossier>`
    /// est sondé quand même — la structure disque est unifiée.
    #[test]
    fn la_sonde_auto_cible_l_artefact_hors_cargo()
      {
        let dir = repertoire_temp("auto-nocargo");
        let nom = dir.file_name().unwrap().to_string_lossy().to_string();
        ecrire_binaire(
          &dir.join("target").join("debug").join(&nom),
          r#"echo "1.2.3_Debug (01-02-2024)""#,
        );
        let lv = lire_variante_avec_sonde(&dir, "debug", None).expect("artefact lisible");
        assert_eq!(lv.etat.version, "1.2.3");
        assert_eq!(lv.etat.statut, "Debug");
        assert_eq!(lv.etat.date, "01-02-2024");
        // Release absente : rien.
        assert!(lire_variante_avec_sonde(&dir, "release", None).is_none());
        let _ = std::fs::remove_dir_all(&dir);
      }

    /// Une commande de sonde personnalisée est exécutée depuis la racine du
    /// projet (le script relit un fichier local pour le prouver).
    #[test]
    fn la_sonde_personnalisee_s_execute_depuis_la_racine_du_projet()
      {
        let dir = repertoire_temp("sonde-cmd");
        std::fs::write(dir.join("etat-courant.txt"), "7.7.7_Stable (11-11-2025)").unwrap();
        let lv = lire_variante_avec_sonde(
          &dir,
          "release",
          Some("cat etat-courant.txt"),
        )
        .expect("sortie lisible");
        assert_eq!(lv.etat.version, "7.7.7");
        assert_eq!(lv.etat.statut, "Stable");
        assert_eq!(lv.etat.date, "11-11-2025");
        let _ = std::fs::remove_dir_all(&dir);
      }

    /// mreport publie son `-V` sur stderr (avec `\r` de fin de ligne) : les
    /// deux flux doivent être lus, la sortie d'erreur comprise.
    #[test]
    fn la_sonde_lit_aussi_la_sortie_d_erreur()
      {
        let dir = repertoire_temp("sonde-stderr");
        ecrire_binaire(
          &dir.join("tool"),
          r#"printf '3.0.0_Beta-Release (05-06-2026)\r\n' 1>&2"#,
        );
        let lv = lire_variante_avec_sonde(&dir, "debug", Some("./tool")).expect("stderr lu");
        assert_eq!(lv.etat.version, "3.0.0");
        assert_eq!(lv.etat.statut, "Beta-Release");
        assert_eq!(lv.etat.date, "05-06-2026");
        let _ = std::fs::remove_dir_all(&dir);
      }

    /// Deux projets, deux variantes, deux commandes distinctes : la sonde
    /// réglable reste indépendante par variante.
    #[test]
    fn les_sondes_s_independisent_par_variante()
      {
        let dir = repertoire_temp("double");
        ecrire_binaire(&dir.join("out").join("a"), r#"echo "1.0.0_Debug (01-01-2026)""#);
        ecrire_binaire(&dir.join("out").join("b"), r#"echo "2.0.0_Release (02-02-2026)""#);
        let debug = lire_variante_avec_sonde(&dir, "debug", Some("out/a"))
          .expect("debug lisible");
        let release = lire_variante_avec_sonde(&dir, "release", Some("out/b"))
          .expect("release lisible");
        assert_eq!(debug.etat.version, "1.0.0");
        assert_eq!(release.etat.version, "2.0.0");
        assert_ne!(debug.empreinte, release.empreinte);
        let _ = std::fs::remove_dir_all(&dir);
      }
  }
