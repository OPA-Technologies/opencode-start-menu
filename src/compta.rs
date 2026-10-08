use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::perms;
use crate::registry::ProjectEntry;

/// Classe comptable d'une session, fixée au moment du lancement.
///
/// Elle ne change jamais ensuite : c'est elle qui décidera (G3) ce qui est
/// facturé ou non, et auquel des projets. Le classement est enregistré dans
/// `sessions_comptes.json` pour rester stable même si le registre bouge
/// (renommage, archivage, déplacement de bibliothèque).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClasseDeSession
  {
    /// Le répertoire est exactement celui d'un projet ordinaire.
    Projet,

    /// Le répertoire est une sous-arborescence d'un projet (module).
    Module,

    /// Le répertoire est exactement celui d'une bibliothèque.
    Bibliotheque,

    /// Session « Sans projet » (touche `v`) dans `$HOME` : jamais comptée.
    Libre,

    /// Répertoire sans aucune attache dans le registre.
    Orphelin,
  }

impl ClasseDeSession
  {
    pub(crate) fn libelle(self) -> &'static str
      {
        match self
          {
            ClasseDeSession::Projet => "Projet",
            ClasseDeSession::Module => "Module",
            ClasseDeSession::Bibliotheque => "Bibliotheque",
            ClasseDeSession::Libre => "Libre",
            ClasseDeSession::Orphelin => "Orphelin",
          }
      }

    /// Un libellé inconnu (fichier édité à la main, ancien format…) retombe
    /// sur [`ClasseDeSession::Orphelin`], le plus prudent : rien n'est
    /// attaché à un projet.
    #[cfg(test)]
    fn de_libelle(s: &str) -> ClasseDeSession
      {
        match s
          {
            "Projet" => ClasseDeSession::Projet,
            "Module" => ClasseDeSession::Module,
            "Bibliotheque" => ClasseDeSession::Bibliotheque,
            "Libre" => ClasseDeSession::Libre,
            _ => ClasseDeSession::Orphelin,
          }
      }
  }

/// Une ligne de `sessions_comptes.json` : un lancement, horodaté.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct EntreeSession
  {
    /// Chemin du répertoire lancé, tel qu'affiché par le menu.
    pub repertoire: String,

    /// Libellé de la classe ([`ClasseDeSession::libelle`]).
    pub type_session: String,

    /// Id du projet dont la session relève (projet ou module).
    pub projet_id: Option<String>,

    /// Id de la bibliothèque (session projet d'une bibliothèque, ou module
    /// d'une bibliothèque).
    pub lib_id: Option<String>,

    /// Horodatage du lancement en millisecondes (epoch).
    pub horodatage_ms: i64,
  }

impl EntreeSession
  {
    #[cfg(test)]
    pub(crate) fn classe(&self) -> ClasseDeSession
      {
        ClasseDeSession::de_libelle(&self.type_session)
      }
  }

/// La table comptable vit à côté du registre des projets.
///
/// déduit de `registre` (`…/opencode/projects.json` → `…/opencode/
/// sessions_comptes.json`) pour que les tests puissent pointer vers un
/// répertoire temporaire exactement comme pour le registre.
pub(crate) fn chemin_table(registre: &Path) -> PathBuf
  {
    registre.with_file_name("sessions_comptes.json")
  }

fn maintenant_ms() -> i64
  {
    std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map(|d| d.as_millis() as i64)
      .unwrap_or(0)
  }

/// Lit la table. Une table absente est une liste vide ; une table illisible
/// est ignorée plutôt que de faire échouer un lancement.
pub(crate) fn charger_table(chemin: &Path) -> Result<Vec<EntreeSession>>
  {
    if !chemin.exists()
      {
        return Ok(Vec::new());
      }
    let contenu = std::fs::read_to_string(chemin)?;
    Ok(serde_json::from_str(&contenu).unwrap_or_default())
  }

/// Ajoute un lancement à la table, en mode best-effort : un échec d'écriture
/// ne doit jamais bloquer le lancement d'opencode (la comptabilité est un
/// outil, pas une barrière).
pub(crate) fn enregistrer_lancement(chemin: &Path, entree: &EntreeSession)
  {
    let mut table = charger_table(chemin).unwrap_or_default();
    table.push(entree.clone());
    let contenu = match serde_json::to_string_pretty(&table)
      {
        Ok(c) => c,
        Err(_) => return,
      };
    let _ = perms::create_private(chemin, contenu.as_bytes());
    let _ = perms::private_file(chemin);
  }

fn meme_repertoire(a: &Path, b: &Path) -> bool
  {
    if a == b
      {
        return true;
      }
    match (a.canonicalize(), b.canonicalize())
      {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
      }
  }

/// Classe le répertoire lancé à partir du registre, sans toucher au disque.
///
/// Règles, dans l'ordre :
///   1. session « Sans projet » → [`ClasseDeSession::Libre`] ;
///   2. chemin égal à un projet → `Projet` (ou `Bibliotheque` si le drapeau
///      `est_bibliotheque` est posé) ;
///   3. chemin strictement sous un projet → `Module`, rattaché à l'ancêtre
///      le plus profond (un projet imbriqué l'emporte sur son contenant) ;
///   4. rien de tout ça → [`ClasseDeSession::Orphelin`].
pub(crate) fn classer_repertoire(
  projets: &[ProjectEntry],
  dir: &Path,
  bare: bool,
) -> EntreeSession
  {
    let entree = EntreeSession {
      repertoire: dir.to_string_lossy().to_string(),
      type_session: ClasseDeSession::Libre.libelle().to_string(),
      projet_id: None,
      lib_id: None,
      horodatage_ms: maintenant_ms(),
    };
    if bare
      {
        return entree;
      }

    for p in projets
      {
        if meme_repertoire(&p.path, dir)
          {
            let (classe, projet_id, lib_id) = if p.est_bibliotheque
              {
                (ClasseDeSession::Bibliotheque, None, Some(p.id.clone()))
              }
            else
              {
                (ClasseDeSession::Projet, Some(p.id.clone()), None)
              };
            return EntreeSession {
              repertoire: entree.repertoire,
              type_session: classe.libelle().to_string(),
              projet_id,
              lib_id,
              horodatage_ms: entree.horodatage_ms,
            };
          }
      }

    // Ancêtre le plus profond = le plus de composants de chemin.
    let ancetre = projets
      .iter()
      .filter(|p| dir.starts_with(&p.path) && dir != p.path)
      .max_by_key(|p| p.path.components().count());
    if let Some(p) = ancetre
      {
        let (projet_id, lib_id) = if p.est_bibliotheque
          {
            (None, Some(p.id.clone()))
          }
        else
          {
            (Some(p.id.clone()), None)
          };
        return EntreeSession {
          repertoire: entree.repertoire,
          type_session: ClasseDeSession::Module.libelle().to_string(),
          projet_id,
          lib_id,
          horodatage_ms: entree.horodatage_ms,
        };
      }

    EntreeSession {
      repertoire: entree.repertoire,
      type_session: ClasseDeSession::Orphelin.libelle().to_string(),
      projet_id: None,
      lib_id: None,
      horodatage_ms: entree.horodatage_ms,
    }
  }

#[cfg(test)]
mod tests
  {
    use std::path::PathBuf;

    use super::{
      charger_table, chemin_table, classer_repertoire, enregistrer_lancement,
      ClasseDeSession, EntreeSession,
    };
    use crate::registry::ProjectEntry;

    fn temp(nom: &str) -> PathBuf
      {
        let dir = std::env::temp_dir()
            .join(format!("op-compta-{}-{nom}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
      }

    fn entre_rapide(projet_id: &str) -> ProjectEntry
      {
        ProjectEntry {
            id: projet_id.to_string(),
            name: projet_id.to_string(),
            path: temp(projet_id),
            description: String::new(),
            status: String::new(),
            version: String::new(),
            token: String::new(),
            used_pct: String::new(),
            spent: String::new(),
            last_model: String::new(),
            modified: String::new(),
            total_tokens: String::new(),
            total_cost: String::new(),
            session_count: String::new(),
            last_session_tokens: String::new(),
            last_session_cost: String::new(),
            last_session_cost_local: String::new(),
            est_bibliotheque: false,
            last_agent: String::new(),
            last_time_created: String::new(),
            last_session_context_pct: String::new(),
            last_duree: String::new(),
            total_duree: String::new(),
            last_valorisation: String::new(),
            total_valorisation: String::new(),
            tarif_resume: String::new(),
            status_debug: String::new(),
            version_debug: String::new(),
            status_release: String::new(),
            version_release: String::new(),
            debug_modified: String::new(),
            release_modified: String::new(),
            last_compile_debug_secs: 0,
            last_compile_release_secs: 0,
            version_manually_edited: false,
            source_hash: String::new(),
            tarif_mode: "web".to_string(),
            tarif_manual: 0.0,
            tarif_web_series: Vec::new(),
            tarif_web_fetched_ms: 0,
            tarif_web_secteur: false,
            tarif_web_pays: String::new(),
        }
      }

    #[test]
    fn bare_est_classe_libre_sans_attache() {
        let dir = temp("bare");
        let e = classer_repertoire(&[], &dir.join("x"), true);
        assert_eq!(e.classe(), ClasseDeSession::Libre);
        assert!(e.projet_id.is_none());
        assert!(e.lib_id.is_none());
    }

    #[test]
    fn un_projet_exact_est_classe_projet_avec_son_id() {
        let dir = temp("exact");
        let p = entre_rapide("maison");
        let p2 = ProjectEntry {
            path: dir.join("autre"),
            ..entre_rapide("autre")
        };
        let chemin = p.path.clone();
        let e = classer_repertoire(&[p2, p], &chemin, false);
        assert_eq!(e.classe(), ClasseDeSession::Projet);
        assert_eq!(e.projet_id.as_deref(), Some("maison"));
        assert!(e.lib_id.is_none());
    }

    #[test]
    fn une_bibliotheque_court_le_champ_lib() {
        let p = entre_rapide("op-tui");
        let p = ProjectEntry { est_bibliotheque: true, ..p };
        let e = classer_repertoire(&[p], &temp("op-tui"), false);
        assert_eq!(e.classe(), ClasseDeSession::Bibliotheque);
        assert!(e.projet_id.is_none());
        assert_eq!(e.lib_id.as_deref(), Some("op-tui"));
    }

    #[test]
    fn un_module_est_classe_module_rattache_a_l_ancetre() {
        let p = entre_rapide("menu");
        let module = p.path.join("src");
        let e = classer_repertoire(&[p], &module, false);
        assert_eq!(e.classe(), ClasseDeSession::Module);
        assert_eq!(e.projet_id.as_deref(), Some("menu"));
    }

    #[test]
    fn l_ancetre_le_plus_profond_gagne() {
        let contenant = entre_rapide("contenant");
        let imbrique = ProjectEntry {
            path: contenant.path.join("sous"),
            ..entre_rapide("imbrique")
        };
        let cible = imbrique.path.join("src");
        let e = classer_repertoire(&[contenant, imbrique], &cible, false);
        assert_eq!(e.classe(), ClasseDeSession::Module);
        assert_eq!(e.projet_id.as_deref(), Some("imbrique"));
    }

    #[test]
    fn le_module_d_une_bibliotheque_est_classe_module_sur_le_champ_lib() {
        let p = entre_rapide("lib");
        let p = ProjectEntry { est_bibliotheque: true, ..p };
        let module = p.path.join("src");
        let e = classer_repertoire(&[p], &module, false);
        assert_eq!(e.classe(), ClasseDeSession::Module);
        assert!(e.projet_id.is_none());
        assert_eq!(e.lib_id.as_deref(), Some("lib"));
    }

    #[test]
    fn un_repertoire_inconnu_est_orphelin() {
        let p = entre_rapide("connu");
        let inconnu = temp("inconnu");
        let e = classer_repertoire(&[p], &inconnu, false);
        assert_eq!(e.classe(), ClasseDeSession::Orphelin);
        assert!(e.projet_id.is_none());
        assert!(e.lib_id.is_none());
    }

    #[test]
    fn une_table_absente_est_vide_et_la_table_suit_le_registre() {
        let registre = temp("registre").join("projects.json");
        let chemin = chemin_table(&registre);
        assert_eq!(
            chemin.file_name().and_then(|n| n.to_str()),
            Some("sessions_comptes.json")
        );
        assert!(charger_table(&chemin).unwrap().is_empty());
    }

    #[test]
    fn un_lancement_enregistre_se_relit() {
        let chemin = chemin_table(&temp("ecrit").join("projects.json"));
        let e = EntreeSession {
          repertoire: "/tmp/essai".to_string(),
          type_session: "Projet".to_string(),
          projet_id: Some("essai".to_string()),
          lib_id: None,
          horodatage_ms: 1_700_000_000_000,
        };
        enregistrer_lancement(&chemin, &e);
        let lus = charger_table(&chemin).unwrap();
        assert_eq!(lus.len(), 1);
        assert_eq!(lus[0].projet_id.as_deref(), Some("essai"));
        enregistrer_lancement(&chemin, &e);
        assert_eq!(charger_table(&chemin).unwrap().len(), 2);
    }

    #[test]
    fn un_libelle_inconnu_retombe_sur_orphelin() {
        let e = EntreeSession {
          repertoire: "/tmp/x".to_string(),
          type_session: "Marche".to_string(),
          projet_id: None,
          lib_id: None,
          horodatage_ms: 0,
        };
        assert_eq!(e.classe(), ClasseDeSession::Orphelin);
    }

    #[test]
    fn le_chemin_canonique_est_rattache_au_projet() {
        let dir = temp("lien");
        let p = ProjectEntry {
          path: dir.canonicalize().unwrap(),
          ..entre_rapide("lié")
        };
        let e = classer_repertoire(&[p], &dir, false);
        assert_eq!(e.classe(), ClasseDeSession::Projet);
    }
  }