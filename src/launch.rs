use anyhow::{anyhow, Result};
use std::{path::Path, process::Command, time::Duration};

use rusqlite::{params_from_iter, Connection, OpenFlags};

use crate::lock;

/// Nature du lancement OpenCode demandé par le menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaunchKind
  {
    /// Relance du projet : verrou, résumé via OPENCODE_AUTO_RESUME et
    /// `opencode --continue` (avec `sudo -i` quand demandé).
    Resume,

    /// Session « à blanc » : `opencode` vierge, sans verrou, sans résumé,
    /// sans todo, sans liste de projets, sans `--continue`, sans sudo.
    Bare,

    /// Premier lancement d'un répertoire : verrou, mais aucun `--continue`.
    /// Reprendre une session qui n'existe pas fait échouer opencode dès le
    /// démarrage (`Expected a string starting with "ses", got "dummy"`),
    /// d'où ce troisième mode déclenché par [`session_existante`].
    Module,
  }

fn shell_quote(s: &str) -> String
  {
    format!("'{}'", s.replace('\'', "'\\''"))
  }

fn report(status: std::process::ExitStatus) -> Result<()>
  {
    if status.success()
      {
        Ok(())
      }
    else
      {
        Err(anyhow!(
          "opencode a échoué (code de sortie {:?})",
          status.code()
        ))
      }
  }

/// Lance `opencode-wrap` sans `--continue` ni résumé.
///
/// Chemin emprunté par [`LaunchKind::Module`] : le répertoire n'a aucune
/// session, rien donc à reprendre, mais `--root` doit continuer de
/// fonctionner comme pour une reprise.
fn lancer_sans_resume(dir: &Path, sudo: bool, project_dir: &str) -> Result<()>
  {
    let status = if sudo
      {
        let cmd = format!(
          "cd {} && OPENCODE_PROJECT_DIR={} opencode-wrap",
          shell_quote(&dir.to_string_lossy()),
          shell_quote(project_dir),
        );
        Command::new("sudo")
          .args(["-i", "sh", "-c", &cmd])
          .status()
          .map_err(|e| anyhow!("échec du lancement d'opencode-wrap : {e}"))?
      }
    else
      {
        Command::new("opencode-wrap")
          .env("OPENCODE_PROJECT_DIR", project_dir)
          .current_dir(dir)
          .status()
          .map_err(|e| anyhow!("échec du lancement d'opencode-wrap : {e}"))?
      };
    report(status)
  }

/// Une session opencode existe-t-elle déjà pour `dir` ?
///
/// Sert à choisir entre [`LaunchKind::Resume`] et [`LaunchKind::Module`] :
/// `--continue` sur un répertoire sans session plante opencode au
/// démarrage, alors qu'un lancement vierge reste inoffensif.
///
/// Toute erreur de lecture compte comme « aucune session » : c'est le
/// seul choix sûr, puisque l'autre option bloque le lancement.
pub(crate) fn session_existante(db_path: &Path, dir: &Path) -> bool
  {
    if !db_path.exists()
      {
        return false;
      }
    // Même repli que `registry::load_usage` : lecture-écriture d'abord,
    // lecture seule ensuite (base en WAL, permissions partielles…).
    let Some(conn) = Connection::open_with_flags(
      db_path,
      OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .or_else(|_| {
      Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
      )
    })
    .ok() else {
      return false;
    };
    let _ = conn.busy_timeout(Duration::from_millis(500));

    // Le verrou canononicalise déjà les chemins : faire de même ici évite
    // de manquer une session enregistrée sous le chemin résolu (lien
    // symbolique) plutôt que sous celui affiché par le menu.
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
    let sql =
      format!("SELECT 1 FROM session WHERE directory IN ({placeholders}) LIMIT 1");
    conn
      .query_row(&sql, params_from_iter(candidats.iter()), |_| Ok(()))
      .is_ok()
  }

/// Lance l'exécutable `opencode` (via le wrapper `opencode-wrap`) dans le dossier `dir`.
///
/// Résume : le TUI est lancé avec `--continue` : il rouvre la dernière
/// session du projet directement dans la vue chat (sinon l'écran d'accueil
/// reste « stealth » tant qu'un texte n'est pas saisi). Les instructions du
/// résumé sont transmises via OPENCODE_AUTO_RESUME : le plugin global
/// projects-starter.ts les détecte, injecte la bulle « Generating Resume
/// and Todo... » (visible) et cache les instructions dans le champ
/// « system » du prompt.
///
/// Vierge : `opencode-wrap` est lancé sans argument dans `dir` ; aucun résumé,
/// aucune todo, aucune liste de projets, aucune session à rouvrir.
///
/// Le wrapper `opencode-wrap` capture le % de contexte à la sortie d'opencode
/// et l'enregistre dans la base SQLite pour l'affichage dans opencode-start-menu.
pub(crate) fn run_launch_sequence(
  kind: LaunchKind,
  name: &str,
  dir: &Path,
  sudo: bool,
) -> Result<()>
  {
    // Verrou : indispensable dès qu'une session peut toucher le registre
    // du projet (empêche deux lancements simultanés du même répertoire).
    // Inutile pour une session vierge : aucun registre n'est touché, on ne
    // fait que lancer opencode.
    let _lock = match kind
      {
        LaunchKind::Resume | LaunchKind::Module => Some(lock::acquire(dir)?),
        LaunchKind::Bare => None,
      };

    if matches!(kind, LaunchKind::Resume | LaunchKind::Module)
        && sudo
        && !cfg!(unix)
      {
        return Err(anyhow!(
          "le mode --root (sudo) n'est pas supporté sur cette plateforme"
        ));
      }

    // Utiliser opencode-wrap au lieu d'opencode directement
    // Le wrapper capture le % contexte à la sortie et met à jour la DB.
    let dir_str = dir.to_string_lossy().to_string();
    let project_dir = dir_str.clone();

    match kind
      {
        LaunchKind::Bare =>
          {
            let status = Command::new("opencode-wrap")
              .current_dir(dir)
              .status()
              .map_err(|e| anyhow!("échec du lancement d'opencode-wrap : {e}"))?;
            report(status)
          }

        // Aucune session à reprendre : même exécution qu'une session
        // vierge, mais le verrou est posé et `--root` reste honoré.
        LaunchKind::Module => lancer_sans_resume(dir, sudo, &project_dir),

        LaunchKind::Resume =>
          {
            let prompt = format!(
              "Résumé d'avancement du projet « {name} ».\n\
              Produis un résumé concis et structuré :\n\
              1. Où en est le projet (statut, dernières modifications, tâches récentes).\n\
              2. Points de vigilance éventuels.\n\
              3. Prochaines étapes : crée une todo list (todowrite) des 3 à 5\n\
              actions concrètes à faire ensuite.\n\
              Utilise git log --oneline -15, git status, git diff --stat, ls,\n\
              et les fichiers de config. Réponds en français, format court."
            );

            let status = if sudo
              {
                // Avec sudo, on passe le chemin du projet via variable d'environnement
                // pour que le wrapper sache quelle session mettre à jour
                let cmd = format!(
                  "cd {} && OPENCODE_AUTO_RESUME={} OPENCODE_PROJECT_DIR={} opencode-wrap --continue",
                  shell_quote(&dir.to_string_lossy()),
                  shell_quote(&prompt),
                  shell_quote(&project_dir),
                );
                Command::new("sudo")
                  .args(["-i", "sh", "-c", &cmd])
                  .status()
                  .map_err(|e| anyhow!("échec du lancement d'opencode-wrap : {e}"))?
              }
            else
              {
                Command::new("opencode-wrap")
                  .env("OPENCODE_AUTO_RESUME", &prompt)
                  .env("OPENCODE_PROJECT_DIR", &project_dir)
                  .arg("--continue")
                  .current_dir(dir)
                  .status()
                  .map_err(|e| anyhow!("échec du lancement d'opencode-wrap : {e}"))?
              };
            report(status)
          }
      }
  }

#[cfg(test)]
mod tests
  {
    use std::path::{Path, PathBuf};

    use rusqlite::Connection;

    use super::{session_existante, shell_quote};

    /// Répertoire de test isolé, à la manière de `lock::tests`.
    fn temp(nom: &str) -> PathBuf
      {
        let dir = std::env::temp_dir()
            .join(format!("op-launch-{}-{nom}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
      }

    /// Base `opencode.db` vierge, contenant seulement la table `session`.
    fn base_vide(dir: &Path) -> PathBuf
      {
        let db = dir.join("opencode.db");
        let conn = Connection::open(&db).unwrap();
        conn
          .execute(
            "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT)",
            [],
          )
          .unwrap();
        drop(conn);
        db
      }

    #[test]
    fn shell_quote_basic() {
        assert_eq!(shell_quote("simple"), "'simple'");
    }

    #[test]
    fn shell_quote_apostrophe() {
        assert_eq!(shell_quote("l'été"), "'l'\\''été'");
    }

    #[test]
    fn shell_quote_special_chars() {
        assert_eq!(shell_quote("a b;$(rm)&\""), "'a b;$(rm)&\"'");
    }

    #[test]
    fn shell_quote_empty() {
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn base_absente_signifie_aucune_session() {
        let dir = temp("absente");
        assert!(
            !session_existante(&dir.join("opencode.db"), &dir),
            "sans base, aucun --continue ne doit être tenté"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn base_sans_ligne_signifie_aucune_session() {
        let dir = temp("vide");
        let db = base_vide(&dir);
        assert!(
            !session_existante(&db, &dir),
            "table vide = premier lancement"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn la_session_est_trouvee_pour_le_bon_repertoire_seul() {
        let cible = temp("cible");
        let autre = temp("autre");
        let db = base_vide(&cible);
        {
            let conn = Connection::open(&db).unwrap();
            conn
              .execute(
                "INSERT INTO session (id, directory) VALUES ('ses_1', ?1)",
                [cible.to_string_lossy().as_ref()],
              )
              .unwrap();
        }
        assert!(
            session_existante(&db, &cible),
            "session enregistrée = reprise autorisée"
        );
        assert!(
            !session_existante(&db, &autre),
            "un autre répertoire sans session ne doit pas être repris"
        );
        let _ = std::fs::remove_dir_all(&cible);
        let _ = std::fs::remove_dir_all(&autre);
    }

    #[test]
    fn la_session_enregistree_via_le_chemin_canonique_est_trouvee() {
        let dir = temp("lien");
        let db = base_vide(&dir);
        let abs = dir.canonicalize().unwrap();
        {
            let conn = Connection::open(&db).unwrap();
            conn
              .execute(
                "INSERT INTO session (id, directory) VALUES ('ses_1', ?1)",
                [abs.to_string_lossy().as_ref()],
              )
              .unwrap();
        }
        assert!(
            session_existante(&db, &dir),
            "le chemin canonique doit être rapproché du chemin affiché"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
  }
