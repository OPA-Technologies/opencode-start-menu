use std::collections::{BTreeMap, HashMap, HashSet};
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
    let t0 = depart_ms;
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

/// Commits d'un dépôt git, en triplés (secondes époque, lignes ajoutées,
/// lignes supprimées) issus de `git log --no-merges --numstat`. Les fichiers
/// binaires (`-`), les lignes d'en-tête et les dépôts sans git sont ignorés.
fn commits_depot(depot: &Path) -> CommitsDepot
  {
    let Ok(sortie) = std::process::Command::new("git")
      .arg("-C")
      .arg(depot)
      .arg("log")
      .arg("--no-merges")
      .arg("--numstat")
      .arg("--format=SEP%x1f%ct")
      .output()
    else {
      return Vec::new();
    };
    if !sortie.status.success()
      {
        return Vec::new();
      }
    let texte = String::from_utf8_lossy(&sortie.stdout);
    let mut commits = Vec::new();
    let mut horodatage: Option<i64> = None;
    for ligne in texte.lines()
      {
        // En-tête de commit : `SEP<separateur><ct>`. Il porte l'horodatage
        // époque (secondes) qui s'applique aux statistiques suivantes.
        if let Some(ct) = ligne.split('\u{1f}').nth(1)
          {
            horodatage = ct.trim().parse().ok();
            continue;
          }
        let mut morceaux = ligne.split('\t');
        let (Some(ajoute), Some(supprime)) = (morceaux.next(), morceaux.next())
          else {
          continue;
        };
        let (Ok(ajoute), Ok(supprime)) =
          (ajoute.trim().parse::<u64>(), supprime.trim().parse::<u64>())
          else {
          // Statistique non numérisable (binaire « - ») : ignorée.
          continue;
        };
        let Some(ct) = horodatage else {
          continue;
        };
        commits.push((ct, ajoute, supprime));
      }
    commits
  }

/// Durées (ms) des sessions, dérivées de la table `message` (miroir de
/// `registry::session_durations`). Les sessions sans message sont absentes.
fn durees_des_sessions(conn: &Connection) -> HashMap<String, u64>
  {
    let mut durees = HashMap::new();
    let Ok(mut stmt) = conn.prepare(
      "SELECT session_id, MIN(time_created), MAX(time_updated) \
       FROM message \
       WHERE time_updated IS NOT NULL AND time_created IS NOT NULL \
       GROUP BY session_id",
    ) else {
      return durees;
    };
    if let Ok(lignes) = stmt.query_map([], |l| {
      Ok((
        l.get::<_, String>(0)?,
        l.get::<_, i64>(1)?,
        l.get::<_, i64>(2)?,
      ))
    })
      {
        for (sid, t0, t1) in lignes.flatten()
          {
            durees.insert(sid, (t1 - t0).max(0) as u64);
          }
      }
    durees
  }

/// Triplés (secondes époque, lignes ajoutées, lignes supprimées) d'un dépôt.
type CommitsDepot = Vec<(i64, u64, u64)>;

/// Somme [`DeltaLignes`] des commits d'un dépôt tombant dans la fenêtre
/// [t0, t1] (secondes époque).
fn sommes_commits(commits: &[(i64, u64, u64)], t0: i64, t1: i64) -> DeltaLignes
  {
    let mut delta = DeltaLignes::default();
    for &(ct, ajoute, supprime) in commits
      {
        if ct < t0 || ct > t1
          {
            continue;
          }
        delta.ajoute += ajoute;
        delta.supprime += supprime;
      }
    delta
  }

/// Identifiant de modèle lisible depuis la valeur `model` emmagasinée par
/// opencode (`{"provider" : …, "id" : …}`), sinon la chaîne brute.
fn modele_id(model: &str) -> String
  {
    serde_json::from_str::<serde_json::Value>(model)
      .ok()
      .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()))
      .unwrap_or_else(|| model.to_string())
  }

/// Complément « git » : reconstruit la trace comptable des sessions
/// antérieures à la comptabilisation par constat. Les commits du dépôt du
/// répertoire lancé sont attribués au « projet » ; ceux des bibliothèques
/// tombant dans la fenêtre de la session, à la bibliothèque touchée.
///
/// Seules les sessions sans trace (ni constat, ni git) sont reconstruites :
/// une trace ne se réécrit jamais. Sont ignorées les sessions dont le
/// répertoire n'est ni un projet ni une bibliothèque du registre
/// (historique non compté) et celles sans durée connue. Retourne le nombre
/// d'apports ajoutés.
pub(crate) fn reconstruire_historique_git(
  db: &Path,
  projets: &[ProjectEntry],
  chemin: &Path,
) -> Result<usize>
  {
    let Some(conn) = (if db.exists()
      {
        Connection::open_with_flags(
          db,
          OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .ok()
      }
    else
      {
        None
      })
    else {
      return Ok(0);
    };

    // Toute session déjà tracée (à quelque source que ce soit) est écartée.
    let deja: HashSet<String> = charger_apports(chemin)?
      .iter()
      .map(|a| a.session_id.clone())
      .collect();

    let a_cout_local = conn
      .prepare("SELECT cost_local FROM session LIMIT 1")
      .is_ok();
    let champ_cout = if a_cout_local
      {
        "COALESCE(cost_local, 0)"
      }
    else
      {
        "cost"
      };
    let durees = durees_des_sessions(&conn);
    let requete = format!(
      "SELECT id, directory, {champ_cout}, COALESCE(model,''), time_created \
       FROM session WHERE directory IS NOT NULL AND directory != '' \
       ORDER BY time_created"
    );
    let Ok(mut stmt) = conn.prepare(&requete) else {
      return Ok(0);
    };

    // Commits de chaque bibliothèque du registre, lus une seule fois.
    let lib_commits: Vec<(String, PathBuf, CommitsDepot)> = projets
      .iter()
      .filter(|p| p.est_bibliotheque)
      .filter_map(|p| {
        canonical(&p.path)
          .map(|c| (p.id.clone(), c.clone(), commits_depot(&c)))
      })
      .collect();

    let Ok(mut lignes) = stmt.query_map([], |l| {
      Ok((
        l.get::<_, String>(0)?,
        l.get::<_, String>(1)?,
        l.get::<_, f64>(2)?,
        l.get::<_, String>(3)?,
        l.get::<_, i64>(4)?,
      ))
    }) else {
      return Ok(0);
    };

    // Cache des commits par dépôt projet : un dépôt n'est sondé qu'une fois.
    let mut cache_depots: HashMap<PathBuf, CommitsDepot> = HashMap::new();
    let mut ajoutes = 0usize;
    while let Ok(Some((session_id, repertoire, cout_local, modele, t0_s))) =
      lignes.next().transpose()
      {
        if deja.contains(&session_id)
          {
            continue;
          }
        let Some(depot) = canonical(Path::new(&repertoire)) else {
          continue;
        };
        let entree = classer_repertoire(projets, Path::new(&repertoire), false);
        if !matches!(entree.type_session.as_str(), "Projet" | "Bibliotheque")
          {
            continue;
          }
        let Some(&duree_ms) = durees.get(&session_id) else {
          continue;
        };
        if duree_ms == 0
          {
            continue;
          }
        // `time_created` est en millisecondes (unité réelle d'opencode),
        // comme `duree_ms` (dérivée des messages) ; les commits git sont
        // eux en secondes époque : on ramène la fenêtre en secondes.
        let t0_ms = t0_s;
        let t1_ms = t0_ms + duree_ms as i64;
        let t0_s = t0_ms / 1000;
        let t1_s = t1_ms / 1000;

        let mut deltas: BTreeMap<String, DeltaLignes> = BTreeMap::new();
        let commits_projet = cache_depots
          .entry(depot.clone())
          .or_insert_with(|| commits_depot(&depot));
        let delta_projet = sommes_commits(commits_projet, t0_s, t1_s);
        if delta_projet.brut() > 0
          {
            deltas.insert("projet".to_string(), delta_projet);
          }
        for (lib_id, lib_rep, commits) in &lib_commits
          {
            if lib_rep == &depot
              {
                // Le dépôt lancé est déjà compté comme « projet ».
                continue;
              }
            let delta = sommes_commits(commits, t0_s, t1_s);
            if delta.brut() > 0
              {
                deltas.insert(lib_id.clone(), delta);
              }
          }
        let total_brut: u64 = deltas.values().map(DeltaLignes::brut).sum();
        if total_brut == 0
          {
            continue;
          }

        let mut part_lib = BTreeMap::new();
        let mut credit_lib = BTreeMap::new();
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

        let apport = ApportSession {
          session_id,
          repertoire,
          classe: entree.type_session,
          projet_id: entree.projet_id,
          lib_id: entree.lib_id,
          depart_ms: t0_ms,
          fin_ms: t1_ms,
          duree_ms,
          modele: modele_id(&modele),
          cout_local,
          deltas,
          total_brut,
          part_lib,
          credit_lib,
          source: "git".to_string(),
        };
        enregistrer_apport(chemin, &apport)?;
        ajoutes += 1;
      }
    Ok(ajoutes)
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
      reconstruire_historique_git,
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
              depart_ms
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
            date_debug: String::new(),
            date_release: String::new(),
            probe_debug: String::new(),
            probe_release: String::new(),
            build_debug: String::new(),
            build_release: String::new(),
            debug_modified: String::new(),
            release_modified: String::new(),
            last_compile_debug_secs: 0,
            last_compile_release_secs: 0,
            last_target_debug_secs: 0,
            last_target_release_secs: 0,
            debug_snapshot: None,
            release_snapshot: None,
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

    /// Initialise un dépôt git avec un commit, retourne l'horodatage époque.
    fn git_commit(depot: &std::path::Path, message: &str) -> i64
      {
        let _ = std::process::Command::new("git")
          .arg("-C")
          .arg(depot)
          .arg("init")
          .arg("-q")
          .status();
        let _ = std::process::Command::new("git")
          .arg("-C")
          .arg(depot)
          .arg("add")
          .arg("-A")
          .status();
        let commit = std::process::Command::new("git")
          .arg("-C")
          .arg(depot)
          .arg("-c")
          .arg("user.name=test")
          .arg("-c")
          .arg("user.email=test@test")
          .arg("commit")
          .arg("-q")
          .arg("-m")
          .arg(message)
          .output();
        if commit.map(|o| o.status.success()).unwrap_or(false)
          {
            let sortie = std::process::Command::new("git")
              .arg("-C")
              .arg(depot)
              .arg("log")
              .arg("-1")
              .arg("--format=%ct")
              .output();
            if let Ok(out) = sortie
              && let Ok(ct) = String::from_utf8_lossy(&out.stdout).trim().parse::<i64>()
              {
                return ct;
              }
          }
        0
      }

    /// Base avec les tables `session` et `message`, couvrant la session dans
    /// une fenêtre de 90 secondes autour de `t0_s`.
    fn base_git(db: &std::path::Path, dir: &std::path::Path, id: &str, t0_s: i64)
      {
        let conn = Connection::open(db).unwrap();
        conn
          .execute_batch(
            "CREATE TABLE IF NOT EXISTS session (
               id TEXT PRIMARY KEY, directory TEXT, cost REAL,
               cost_local REAL, model TEXT, time_created INTEGER
             );
             CREATE TABLE IF NOT EXISTS message (
               session_id TEXT, time_created INTEGER, time_updated INTEGER
             );",
          )
          .unwrap();
        conn
          .execute(
            "INSERT INTO session (id, directory, cost, cost_local, model, time_created)
             VALUES (?1, ?2, 10.0, 12.5, ?3, ?4)",
            rusqlite::params![
              id,
              dir.to_string_lossy().to_string(),
              r#"{"provider":"test","id":"gpt-test"}"#,
              t0_s * 1000
            ],
          )
          .unwrap();
        conn
          .execute(
            "INSERT INTO message (session_id, time_created, time_updated)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![id, t0_s * 1000, t0_s * 1000 + 90_000],
          )
          .unwrap();
      }

    fn apport_constat(chemin_apports: &std::path::Path, session_id: &str)
      {
        let apport = crate::apport::ApportSession {
          session_id: session_id.to_string(),
          repertoire: "/tmp/deja".to_string(),
          classe: "Projet".to_string(),
          projet_id: Some("deja".to_string()),
          lib_id: None,
          depart_ms: 0,
          fin_ms: 0,
          duree_ms: 0,
          modele: String::new(),
          cout_local: 1.0,
          deltas: BTreeMap::new(),
          total_brut: 0,
          part_lib: BTreeMap::new(),
          credit_lib: BTreeMap::new(),
          source: "constat".to_string(),
        };
        super::enregistrer_apport(chemin_apports, &apport).unwrap();
      }

    #[test]
    fn l_historique_git_reconstruit_la_trace_de_la_session() {
        let (registre, chantier) = chantier("git-projet");
        let depot = chantier.join("proj-git");
        std::fs::create_dir_all(&depot).unwrap();
        std::fs::write(depot.join("a.rs"), "1\n2\n").unwrap();
        let ct = git_commit(&depot, "premier commit");
        std::fs::write(depot.join("a.rs"), "1\n2\n3\n").unwrap();
        let _ct2 = git_commit(&depot, "second commit");
        assert!(ct > 0);

        let db = chantier.join("opencode.db");
        base_git(&db, &depot, "ses_git", ct);
        let chemin = chemin_apports(&registre);
        let n = reconstruire_historique_git(&db, &[projet(&depot)], &chemin).unwrap();
        assert_eq!(n, 1);

        let apports = super::charger_apports(&chemin).unwrap();
        assert_eq!(apports.len(), 1);
        let apport = &apports[0];
        assert_eq!(apport.session_id, "ses_git");
        assert_eq!(apport.source, "git");
        assert_eq!(apport.classe, "Projet");
        assert_eq!(apport.projet_id.as_deref(), Some("projet-test"));
        assert_eq!(apport.deltas["projet"].ajoute, 3);
        assert_eq!(apport.deltas["projet"].supprime, 0);
        assert_eq!(apport.cout_local, 12.5);
        assert_eq!(apport.modele, "gpt-test");
        assert!(apport.credit_lib.is_empty());

        // Idempotence : une seconde lecture n'ajoute et ne réécrit rien.
        assert_eq!(reconstruire_historique_git(&db, &[projet(&depot)], &chemin).unwrap(), 0);
        assert_eq!(super::charger_apports(&chemin).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&chantier);
    }

    #[test]
    fn l_historique_git_ignore_la_session_deja_tracee_par_constat() {
        let (registre, chantier) = chantier("git-deja");
        let depot = chantier.join("proj-deja");
        std::fs::create_dir_all(&depot).unwrap();
        std::fs::write(depot.join("b.rs"), "1\n").unwrap();
        let ct = git_commit(&depot, "commit");
        let db = chantier.join("opencode.db");
        base_git(&db, &depot, "ses_constatee", ct);
        let chemin = chemin_apports(&registre);
        apport_constat(&chemin, "ses_constatee");

        let n = reconstruire_historique_git(&db, &[projet(&depot)], &chemin).unwrap();
        assert_eq!(n, 0);
        let apports = super::charger_apports(&chemin).unwrap();
        assert_eq!(apports.len(), 1);
        assert_eq!(apports[0].source, "constat");
        let _ = std::fs::remove_dir_all(&chantier);
    }

    #[test]
    fn l_historique_git_attribue_les_commits_de_la_bibliotheque() {
        let (registre, chantier) = chantier("git-lib");
        let depot = chantier.join("proj-lib");
        std::fs::create_dir_all(&depot).unwrap();
        std::fs::write(depot.join("a.rs"), "1\n").unwrap();
        let ct = git_commit(&depot, "projet");
        let lib_dir = temp("git-lib-lib");
        std::fs::write(lib_dir.join("lib.rs"), "1\n2\n").unwrap();
        let ct_lib = git_commit(&lib_dir, "lib");
        let mut lib = projet(&lib_dir);
        lib.id = "op-git".to_string();
        lib.est_bibliotheque = true;
        assert!(ct > 0 && ct_lib > 0);

        let db = chantier.join("opencode.db");
        base_git(&db, &depot, "ses_lib", ct);
        let chemin = chemin_apports(&registre);
        let n = reconstruire_historique_git(&db, &[projet(&depot), lib.clone()], &chemin)
          .unwrap();
        assert_eq!(n, 1);

        let apport = &super::charger_apports(&chemin).unwrap()[0];
        assert_eq!(apport.deltas["projet"].ajoute, 1);
        assert_eq!(apport.deltas["op-git"].ajoute, 2);
        assert_eq!(apport.total_brut, 3);
        let p = apport.part_lib["op-git"];
        assert!((p - 2.0 / 3.0).abs() < 1e-9);
        assert!((apport.credit_lib["op-git"] - 12.5 * 2.0 / 3.0).abs() < 1e-9);
        let _ = std::fs::remove_dir_all(&chantier);
        let _ = std::fs::remove_dir_all(&lib_dir);
    }

    #[test]
    fn l_historique_git_ignore_le_repertoire_hors_registre() {
        let (registre, chantier) = chantier("git-orphelin");
        std::fs::write(chantier.join("z.txt"), "1\n").unwrap();
        let ct = git_commit(&chantier, "orphelin");
        let db = chantier.join("opencode.db");
        base_git(&db, &chantier, "ses_or", ct);
        let chemin = chemin_apports(&registre);
        let n = reconstruire_historique_git(&db, &[], &chemin).unwrap();
        assert_eq!(n, 0);
        assert!(super::charger_apports(&chemin).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&chantier);
    }
  }