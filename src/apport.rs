use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::{params_from_iter, Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use crate::compta::classer_repertoire;
use crate::constat::{bilan, constater, BilanLignes};
use crate::perms;
use crate::registry::ProjectEntry;

/// Bilan de lignes d'une arborescence, tel que stocké dans un apport.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DeltaLignes
  {
    pub ajoute: u64,
    pub supprime: u64,
  }

impl DeltaLignes
  {
    fn d_un_bilan(b: BilanLignes) -> Self
      {
        DeltaLignes {
          ajoute: b.ajoute,
          supprime: b.supprime,
        }
      }

    fn brut(&self) -> u64
      {
        self.ajoute + self.supprime
      }
  }

/// Une ligne auditable de `apports.json` : ce qu'une session a produit en
/// deltas de lignes, ce qu'elle a coûté, et la part créditée à chaque
/// bibliothèque. Ne se recompute jamais : c'est la trace, pas un calcul.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ApportSession
  {
    pub session_id: String,
    pub repertoire: String,
    pub classe: String,
    pub projet_id: Option<String>,
    pub lib_id: Option<String>,
    pub depart_ms: i64,
    pub fin_ms: i64,
    pub duree_ms: u64,
    pub modele: String,
    pub cout_local: f64,

    /// Δ bruts par arbre : la clé « projet » désigne le répertoire lancé,
    /// les autres clés sont les ids des bibliothèques touchées.
    pub deltas: BTreeMap<String, DeltaLignes>,

    pub total_brut: u64,

    /// part_lib[l] = Δ bruts de l / Σ Δ bruts de la session.
    pub part_lib: BTreeMap<String, f64>,

    /// credit_lib[l] = coût de la session × part_lib[l]. Le projet conserve
    /// 100 % de son coût (double comptage voulu, non sommable).
    pub credit_lib: BTreeMap<String, f64>,

    /// « constat » : mesuré par snapshots de l'outil. « git » : reconstruit
    /// par commits (complément pour les sessions antérieures).
    pub source: String,
  }

/// Une racine sondée au lancement, avec son propriétaire.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RacineSnapshot
  {
    /// « projet » pour le répertoire lancé, sinon l'id de la bibliothèque.
    detenteur: String,
    constat: BTreeMap<String, crate::constat::EtatFichier>,
  }

/// L'état enregistré juste avant un lancement : le point de départ du diff.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnapshotVigueur
  {
    depart_ms: i64,
    /// Chemin affiché par le menu au moment du lancement.
    repertoire: String,
    racines: BTreeMap<String, RacineSnapshot>,
  }

pub(crate) fn chemin_snapshot(registre: &Path) -> PathBuf
  {
    registre.with_file_name("snapshot_vigueur.json")
  }

pub(crate) fn chemin_apports(registre: &Path) -> PathBuf
  {
    registre.with_file_name("apports.json")
  }

/// Somme des crédits reçus par une bibliothèque : le cumul des parts `p_L`
/// portées à son crédit par les sessions de tous les projets.
pub(crate) fn somme_credits_lib(apports: &[ApportSession], lib_id: &str) -> f64
  {
    apports
      .iter()
      .flat_map(|a| a.credit_lib.get(lib_id))
      .sum()
  }

/// Crédits émis par les sessions d'un projet : ce que ses sessions ont
/// versé au crédit des bibliothèques touchées. Affiché en note, car ces
/// montants ne s'additionnent pas au coût du projet (qui les couvre déjà).
pub(crate) fn somme_credits_emis(apports: &[ApportSession], projet_id: &str) -> f64
  {
    apports
      .iter()
      .filter(|a| a.projet_id.as_deref() == Some(projet_id))
      .flat_map(|a| a.credit_lib.values())
      .sum()
  }

/// Bandeau de démarrage : rappel concis de l'état de la comptabilité.
pub(crate) fn bandeau_debut(apports: &[ApportSession]) -> Option<String>
  {
    let constates = apports.iter().filter(|a| a.source == "constat").count();
    let credits: f64 = apports
      .iter()
      .flat_map(|a| a.credit_lib.values())
      .sum();
    if constates == 0 && credits <= 0.0
      {
        return None;
      }
    let mut msg = format!("Comptabilité : {constates} sessions constatées");
    if credits > 0.0
      {
        msg.push_str(&format!(
          ", {} de crédits bibliothèques (non additionnés)",
          crate::format_currency(credits, crate::LOCAL_CURRENCY)
        ));
      }
    Some(msg)
  }

fn maintenant_ms() -> i64
  {
    std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map(|d| d.as_millis() as i64)
      .unwrap_or(0)
  }

/// Racines à sonder pour un lancement : le répertoire lancé plus chaque
/// bibliothèque du registre. Ce sont elles qui peuvent recevoir des edits
/// pendant la session — le constat doit les couvrir toutes, sinon la part
/// d'une bibliothèque serait inmesurable.
fn racines_a_sonder(projets: &[ProjectEntry], dir: &Path) -> BTreeMap<String, RacineSnapshot>
  {
    let mut racines = BTreeMap::new();
    let dir_canonique = canonical(dir);
    let mut deja: Vec<PathBuf> = Vec::new();
    if let Some(c) = dir_canonique.clone()
      {
        deja.push(c.clone());
        racines.insert(
          c.to_string_lossy().to_string(),
          RacineSnapshot {
            detenteur: "projet".to_string(),
            constat: constater(&c),
          },
        );
      }
    for p in projets
      {
        if !p.est_bibliotheque
          {
            continue;
          }
        let Some(c) = canonical(&p.path) else {
          continue;
        };
        if deja.contains(&c)
          {
            continue;
          }
        deja.push(c.clone());
        racines.insert(
          c.to_string_lossy().to_string(),
          RacineSnapshot {
            detenteur: p.id.clone(),
            constat: constater(&c),
          },
        );
      }
    racines
  }

fn canonical(p: &Path) -> Option<PathBuf>
  {
    p.canonicalize().ok().filter(|c| c.is_dir())
  }

/// Constate l'état de départ juste avant que la session ne démarre.
///
/// Best-effort : si l'écriture du snapshot échoue, le lancement continue ;
/// seule la trace comptable de la session sera manquante.
pub(crate) fn demarrer(registre: &Path, projets: &[ProjectEntry], dir: &Path)
  {
    let racines = racines_a_sonder(projets, dir);
    let snap = SnapshotVigueur {
      depart_ms: maintenant_ms(),
      repertoire: dir.to_string_lossy().to_string(),
      racines,
    };
    let Ok(contenu) = serde_json::to_string_pretty(&snap) else {
      return;
    };
    let chemin = chemin_snapshot(registre);
    let _ = perms::create_private(&chemin, contenu.as_bytes());
    let _ = perms::private_file(&chemin);
  }

/// Saisit la dernière session connue de la base pour `dir` depuis `depart_ms`.
///
/// Retourne (id, coût local, modèle). « Dernière » = `time_created` maximal,
/// ce qui désigne la session qui vient de se terminer (procédures de
/// comptabilisation séquentielles, une seule session à la fois).
fn derniere_session(db: &Path, dir: &Path, depart_ms: i64) -> Option<(String, f64, String)>
  {
    if !db.exists()
      {
        return None;
      }
    let conn = Connection::open_with_flags(
      db,
      OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .or_else(|_| {
      Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
      )
    })
    .ok()?;
    let _ = conn.busy_timeout(std::time::Duration::from_millis(500));

    let mut candidats: Vec<String> = vec![dir.to_string_lossy().to_string()];
    if let Ok(abs) = dir.canonicalize()
      {
        let s = abs.to_string_lossy().to_string();
        if !candidats.contains(&s)
          {
            candidats.push(s);
          }
      }
    let placeholders = vec!["?"; candidats.len()].join(", ");
    let t0 = depart_ms / 1000;
    let requete_exacte = format!(
      "SELECT id, cost, model FROM session \
       WHERE directory IN ({placeholders}) AND time_created >= ? \
       ORDER BY time_created DESC LIMIT 1"
    );
    let mut params: Vec<rusqlite::types::Value> = candidats
      .iter()
      .map(|c| rusqlite::types::Value::Text(c.clone()))
      .collect();
    params.push(rusqlite::types::Value::Integer(t0));
    let lire_exact = |conn: &Connection| -> Option<(String, f64, String)> {
      let mut stmt = conn.prepare(&requete_exacte).ok()?;
      let mut lignes = stmt.query(params_from_iter(params.iter())).ok()?;
      let ligne = lignes.next().ok().flatten()?;
      Some((
        ligne.get::<_, String>(0).ok()?,
        ligne.get::<_, f64>(1).unwrap_or(0.0),
        ligne.get::<_, String>(2).unwrap_or_default(),
      ))
    };
    let (id, cout, modele) = match lire_exact(&conn)
      {
        Some(r) => r,
        // Base ancienne (pas de colonne `time_created` fiable) : dernier
        // résultat sans filtre temporel.
        None =>
          {
            let fallback = format!(
              "SELECT id, cost, COALESCE(model,'') FROM session \
               WHERE directory IN ({placeholders}) ORDER BY time_created DESC LIMIT 1"
            );
            let Ok(mut stmt) = conn.prepare(&fallback) else {
              return None;
            };
            let Ok(mut lignes) = stmt.query(params_from_iter(candidats.iter())) else {
              return None;
            };
            let ligne = lignes.next().ok().flatten()?;
            (
              ligne.get::<_, String>(0).unwrap_or_default(),
              ligne.get::<_, f64>(1).unwrap_or(0.0),
              ligne.get::<_, String>(2).unwrap_or_default(),
            )
          }
      };
    // Le coût local est chargé et converti par `registry::load_usage` lors du
    // rescan post-session ; à défaut, la valeur `cost` (devise d'origine) est
    // conservée telle quelle.
    let cout_local = conn
      .query_row(
        "SELECT COALESCE(cost_local, 0) FROM session WHERE id = ?1",
        [&id],
        |r| r.get(0),
      )
      .unwrap_or(0.0);
    Some((id, if cout_local > 0.0 { cout_local } else { cout }, modele))
  }

pub(crate) fn charger_apports(chemin: &Path) -> Result<Vec<ApportSession>>
  {
    if !chemin.exists()
      {
        return Ok(Vec::new());
      }
    let contenu = std::fs::read_to_string(chemin)?;
    Ok(serde_json::from_str(&contenu).unwrap_or_default())
  }

/// Ajoute (ou remplace, pour la même session et la même source) un apport.
pub(crate) fn enregistrer_apport(chemin: &Path, apport: &ApportSession) -> Result<()>
  {
    let mut apports = charger_apports(chemin)?;
    apports.retain(|a| {
      a.session_id != apport.session_id || a.source != apport.source
    });
    apports.push(apport.clone());
    let contenu = serde_json::to_string_pretty(&apports)?;
    let _ = perms::create_private(chemin, contenu.as_bytes());
    let _ = perms::private_file(chemin);
    Ok(())
  }

/// Diff entre le snapshot posé au lancement et l'état actuel, puis écriture
/// de la trace comptable.
///
/// Retourne un court résumé à présenter dans le menu, ou `None` si aucune
/// session de la base ne correspond (lan­cement échoué, snapshot absent).
pub(crate) fn constater_retour(
  registre: &Path,
  db: &Path,
  projets: &[ProjectEntry],
  dir: &Path,
  bare: bool,
) -> Result<Option<String>>
  {
    let chemin_sn = chemin_snapshot(registre);
    let Ok(contenu) = std::fs::read_to_string(&chemin_sn) else {
      return Ok(None);
    };
    let Ok(snap) = serde_json::from_str::<SnapshotVigueur>(&contenu) else {
      let _ = std::fs::remove_file(&chemin_sn);
      return Ok(None);
    };
    let _ = std::fs::remove_file(&chemin_sn);

    let depart = snap.depart_ms;
    let fin = maintenant_ms();

    let deltas: BTreeMap<String, DeltaLignes> = snap
      .racines
      .iter()
      .map(|(racine, ss)| {
        let constat_apres = constater(Path::new(racine));
        let diff = crate::constat::diff_lignes(&ss.constat, &constat_apres);
        let b = bilan(&diff);
        (ss.detenteur.clone(), DeltaLignes::d_un_bilan(b))
      })
      .collect();
    let total_brut: u64 = deltas.values().map(DeltaLignes::brut).sum();

    let Some((session_id, cout_local, modele)) = derniere_session(db, dir, depart) else {
      return Ok(None);
    };

    let classe = classer_repertoire(projets, dir, bare);
    let exempte = matches!(classe.type_session.as_str(), "Libre" | "Orphelin");
    let mut part_lib = BTreeMap::new();
    let mut credit_lib = BTreeMap::new();
    if !exempte && total_brut > 0
      {
        for (detenteur, delta) in &deltas
          {
            if detenteur == "projet"
              {
                continue;
              }
            let part = delta.brut() as f64 / total_brut as f64;
            part_lib.insert(detenteur.clone(), part);
            credit_lib.insert(detenteur.clone(), cout_local * part);
          }
      }

    let apport = ApportSession {
      session_id,
      repertoire: snap.repertoire,
      classe: classe.type_session,
      projet_id: classe.projet_id,
      lib_id: classe.lib_id,
      depart_ms: depart,
      fin_ms: fin,
      duree_ms: (fin.saturating_sub(depart)) as u64,
      modele,
      cout_local,
      deltas,
      total_brut,
      part_lib,
      credit_lib,
      source: "constat".to_string(),
    };
    enregistrer_apport(&chemin_apports(registre), &apport)?;

    let resume = format!(
      "Constat : +{} −{} lignes{}",
      apport.deltas.get("projet").map(|d| d.ajoute).unwrap_or(0),
      apport.deltas.get("projet").map(|d| d.supprime).unwrap_or(0),
      if apport.credit_lib.is_empty()
        {
          String::new()
        }
      else
        {
          format!(
            " (crédits {} €)",
            apport
              .credit_lib
              .values()
              .map(|c| format!("{c:.2}"))
              .collect::<Vec<_>>()
              .join(" + ")
          )
        },
    );
    Ok(Some(resume))
  }

#[cfg(test)]
mod tests
  {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;

    use rusqlite::Connection;

    use super::{
      chemin_apports, chemin_snapshot, constater_retour, demarrer,
    };
    use crate::compta::{classer_repertoire, ClasseDeSession};
    use crate::registry::ProjectEntry;

    fn temp(nom: &str) -> PathBuf
      {
        let dir = std::env::temp_dir()
            .join(format!("op-apport-{}-{nom}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
      }

    /// Chemin du registre (fichier) et du répertoire projet, côte à côte.
    fn chantier(nom: &str) -> (PathBuf, PathBuf)
      {
        let base = temp(nom);
        (base.join("projects.json"), base)
      }

    /// Base opencode minimale avec une session déjà terminée.
    fn base_session(db: &std::path::Path, dir: &std::path::Path, depart_ms: i64)
      {
        let conn = Connection::open(db).unwrap();
        conn
          .execute(
            "CREATE TABLE IF NOT EXISTS session (
               id TEXT PRIMARY KEY, directory TEXT, cost REAL,
               cost_local REAL, model TEXT, time_created INTEGER
             )",
            [],
          )
          .unwrap();
        conn
          .execute(
            "INSERT INTO session (id, directory, cost, cost_local, model, time_created)
             VALUES ('ses_p1', ?1, 10.0, 12.5, 'gpt-test', ?2)",
            rusqlite::params![
              dir.to_string_lossy().to_string(),
              depart_ms / 1000
            ],
          )
          .unwrap();
      }

    fn base(db: &std::path::Path, dir: &std::path::Path, depart_ms: i64) -> PathBuf
      {
        let chemin_db = db.to_path_buf();
        base_session(&chemin_db, dir, depart_ms);
        chemin_db
      }

    fn projet(chemin: &std::path::Path) -> ProjectEntry
      {
        ProjectEntry {
            id: "projet-test".to_string(),
            name: "projet-test".to_string(),
            path: chemin.to_path_buf(),
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

    /// Départ du lancement simulé : l'instant présent, comme le snapshot.
    fn depart_actuel() -> i64
      {
        std::time::SystemTime::now()
          .duration_since(std::time::UNIX_EPOCH)
          .map(|d| d.as_millis() as i64)
          .unwrap_or(1)
      }

    fn dernier_apport(registre: &std::path::Path)
      {
        let apports = super::charger_apports(&chemin_apports(registre)).unwrap();
        assert_eq!(apports.len(), 1);
      }

    #[test]
    fn sans_snapshot_rien_ne_se_constate() {
        let (registre, chantier) = chantier("absente");
        let db = chantier.join("opencode.db");
        let constat = constater_retour(&registre, &db, &[], &chantier, false).unwrap();
        assert!(constat.is_none());
        let _ = std::fs::remove_dir_all(&chantier);
    }

    #[test]
    fn la_session_finie_est_constatee_et_tracee() {
        let (registre, chantier) = chantier("terminee");
        let db = base(&registre, &chantier, depart_actuel());
        fs::write(chantier.join("a.rs"), "1\n2\n").unwrap();
        fs::write(chantier.join("b.rs"), "3\n").unwrap();
        demarrer(&registre, &[projet(&chantier)], &chantier);
        fs::write(chantier.join("a.rs"), "1\n2\n3\n").unwrap();

        let constat = constater_retour(&registre, &db, &[projet(&chantier)], &chantier, false)
          .unwrap()
          .expect("une session correspondante doit produire un constat");

        let apports = super::charger_apports(&chemin_apports(&registre)).unwrap();
        assert_eq!(apports.len(), 1);
        let apport = &apports[0];
        assert_eq!(apport.session_id, "ses_p1");
        assert_eq!(apport.classe, "Projet");
        assert_eq!(apport.projet_id.as_deref(), Some("projet-test"));
        assert_eq!(apport.total_brut, 1);
        assert_eq!(apport.deltas["projet"].ajoute, 1);
        assert!(apport.credit_lib.is_empty());
        assert!(constat.contains("+1 −0 lignes"));
        assert!(!chemin_snapshot(&registre).exists());
        assert!(apport.fin_ms >= apport.depart_ms);
        assert_eq!(apport.cout_local, 12.5);
        let _ = std::fs::remove_dir_all(&chantier);
    }

    #[test]
    fn une_ligne_retiree_est_comptee_comme_suppression() {
        let (registre, chantier) = chantier("retrait");
        let db = base(&registre, &chantier, depart_actuel());
        fs::write(chantier.join("a.rs"), "1\n2\n3\n4\n").unwrap();
        demarrer(&registre, &[projet(&chantier)], &chantier);
        fs::write(chantier.join("a.rs"), "1\n2\n").unwrap();
        constater_retour(&registre, &db, &[projet(&chantier)], &chantier, false)
          .unwrap()
          .expect("constat attendu");
        dernier_apport(&registre);
        let apport = &super::charger_apports(&chemin_apports(&registre)).unwrap()[0];
        assert_eq!(apport.deltas["projet"].supprime, 2);
        assert_eq!(apport.total_brut, 2);
        assert!(apport.credit_lib.is_empty());
        let _ = std::fs::remove_dir_all(&chantier);
    }

    #[test]
    fn une_bibliotheque_modifiee_recoit_la_part_proportionnelle() {
        let (registre, chantier) = chantier("biblio");
        let lib_dir = temp("biblio-op");
        let mut lib = projet(&lib_dir);
        lib.est_bibliotheque = true;
        lib.id = "op-lib".to_string();
        let db = base(&registre, &chantier, depart_actuel());

        fs::write(chantier.join("app.rs"), "1\n").unwrap();
        fs::write(lib_dir.join("lib.rs"), "1\n").unwrap();
        demarrer(&registre, &[projet(&chantier), lib.clone()], &chantier);
        fs::write(chantier.join("app.rs"), "1\n2\n").unwrap();
        fs::write(lib_dir.join("lib.rs"), "1\n2\n3\n").unwrap();

        constater_retour(&registre, &db, &[projet(&chantier), lib.clone()], &chantier, false)
          .unwrap()
          .expect("constat attendu");
        let apport = &super::charger_apports(&chemin_apports(&registre)).unwrap()[0];
        assert_eq!(apport.deltas["projet"].ajoute, 1);
        assert_eq!(apport.deltas["op-lib"].ajoute, 2);
        assert_eq!(apport.total_brut, 3);
        let p = apport.part_lib["op-lib"];
        assert!((p - 2.0 / 3.0).abs() < 1e-9);
        assert!((apport.credit_lib["op-lib"] - 12.5 * 2.0 / 3.0).abs() < 1e-9);
        let _ = std::fs::remove_dir_all(&chantier);
        let _ = std::fs::remove_dir_all(&lib_dir);
    }

    #[test]
    fn une_session_libre_est_tracee_sans_credit() {
        let (registre, chantier) = chantier("libre");
        let db = base(&registre, &chantier, depart_actuel());
        fs::write(chantier.join("notes.md"), "1\n").unwrap();
        demarrer(&registre, &[], &chantier);
        fs::write(chantier.join("notes.md"), "1\n2\n").unwrap();
        constater_retour(&registre, &db, &[], &chantier, true)
          .unwrap()
          .expect("constat attendu");
        let apport = &super::charger_apports(&chemin_apports(&registre)).unwrap()[0];
        assert_eq!(apport.classe, "Libre");
        assert_eq!(apport.total_brut, 1);
        assert!(apport.credit_lib.is_empty());
        let _ = std::fs::remove_dir_all(&chantier);
    }

    #[test]
    fn un_repertoire_inconnu_est_classe_orphelin_et_non_libre() {
        let chantier = temp("orphelin");
        let e = classer_repertoire(&[], &chantier, false);
        assert_eq!(e.classe(), ClasseDeSession::Orphelin);
        let _ = std::fs::remove_dir_all(&chantier);
    }

    #[test]
    fn les_credits_recus_et_emis_se_somment_par_lib_et_par_projet() {
        let apport = crate::apport::ApportSession {
          session_id: "ses_a".to_string(),
          repertoire: "/tmp/a".to_string(),
          classe: "Projet".to_string(),
          projet_id: Some("alpha".to_string()),
          lib_id: None,
          depart_ms: 0,
          fin_ms: 0,
          duree_ms: 0,
          modele: String::new(),
          cout_local: 12.0,
          deltas: BTreeMap::new(),
          total_brut: 0,
          part_lib: BTreeMap::from([("op-lib".to_string(), 0.5)]),
          credit_lib: BTreeMap::from([("op-lib".to_string(), 6.0)]),
          source: "constat".to_string(),
        };
        assert_eq!(super::somme_credits_lib(std::slice::from_ref(&apport), "op-lib"), 6.0);
        assert_eq!(super::somme_credits_lib(std::slice::from_ref(&apport), "autre"), 0.0);
        assert_eq!(super::somme_credits_emis(std::slice::from_ref(&apport), "alpha"), 6.0);
        assert_eq!(super::somme_credits_emis(std::slice::from_ref(&apport), "gamma"), 0.0);
    }

    #[test]
    fn le_bandeau_ne_s_affiche_qu_avec_du_materiel() {
        assert!(super::bandeau_debut(&[]).is_none());
        let apport = crate::apport::ApportSession {
          session_id: "ses_b".to_string(),
          repertoire: "/tmp/b".to_string(),
          classe: "Projet".to_string(),
          projet_id: Some("alpha".to_string()),
          lib_id: None,
          depart_ms: 0,
          fin_ms: 0,
          duree_ms: 0,
          modele: String::new(),
          cout_local: 12.0,
          deltas: BTreeMap::new(),
          total_brut: 1,
          part_lib: BTreeMap::from([("op-lib".to_string(), 0.5)]),
          credit_lib: BTreeMap::from([("op-lib".to_string(), 6.0)]),
          source: "constat".to_string(),
        };
        let bandeau = super::bandeau_debut(&[apport]).unwrap();
        assert!(bandeau.contains("1 sessions constatées"));
        assert!(bandeau.contains("6,00"));
    }
  }