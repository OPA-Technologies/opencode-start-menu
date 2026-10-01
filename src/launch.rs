use anyhow::{anyhow, Result};
use std::{path::Path, process::Command};

use crate::lock;

/// Nature du lancement OpenCode demandé par le menu.
pub(crate) enum LaunchKind
  {
    /// Relance du projet : verrou, résumé via OPENCODE_AUTO_RESUME et
    /// `opencode --continue` (avec `sudo -i` quand demandé).
    Resume,

    /// Session « à blanc » : `opencode` vierge, sans verrou, sans résumé,
    /// sans todo, sans liste de projets, sans `--continue`, sans sudo.
    Bare,
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
    // Verrou : indispensable pour un résumé (empêche deux lancements
    // simultanés du même projet). Inutile pour une session vierge : aucun
    // registre de projet n'est touché, on ne fait que lancer opencode.
    let _lock = match kind
      {
        LaunchKind::Resume => Some(lock::acquire(dir)?),
        LaunchKind::Bare => None,
      };

    if matches!(kind, LaunchKind::Resume) && sudo && !cfg!(unix)
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
    use super::shell_quote;

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
  }
