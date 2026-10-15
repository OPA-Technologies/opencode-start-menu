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

/// Interroge le binaire d'une variante (`debug`/`release`) de `dir`.
///
/// Renvoie `None` si le binaire n'existe pas, ne répond pas avant 2 s, échoue,
/// ou ne publie aucun état lisible.
pub(crate) fn lire_etat_variante(dir: &Path, profil: &str) -> Option<EtatVariante>
  {
    lire_variante(dir, profil).map(|l| l.etat)
  }

/// Interroge le binaire d'une variante et rend son état et l'empreinte `-V`.
///
/// Même contrat que [`lire_etat_variante`], mais conserve en plus l'empreinte
/// de la sortie : elle sert de signal fin de départage quand deux fiches
/// partagent la même date de dernière mise à jour.
pub(crate) fn lire_variante(dir: &Path, profil: &str) -> Option<LectureVariante>
  {
    let binaire = dir.join("target").join(profil).join(nom_binaire(dir)?);
    if !binaire.is_file()
      {
        return None;
      }
    let sortie = interroger_version(&binaire)?;
    let etat = parser_etat_variante(&sortie)?;
    Some(LectureVariante {
        empreinte: empreinte_v(&sortie),
        etat,
      })
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

/// Lance le binaire avec `-V`, borne à 2 s, et rend sa sortie standard.
fn interroger_version(binaire: &Path) -> Option<String>
  {
    let mut enfant = std::process::Command::new(binaire)
        .arg("-V")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
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
  }
