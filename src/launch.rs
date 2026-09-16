use anyhow::{anyhow, Result};
use std::{path::Path, process::Command};

use crate::lock;

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub(crate) fn run_launch_sequence(name: &str, dir: &Path, sudo: bool) -> Result<()>
  {
    // Le TUI est lancé avec --continue : il rouvre la dernière session du
    // projet directement dans la vue chat (sinon l'écran d'accueil reste
    // « stealth » tant qu'un texte n'est pas saisi). Les instructions du
    // résumé sont transmises via OPENCODE_AUTO_RESUME : le plugin global
    // projects-starter.ts les détecte, injecte la bulle « Generating
    // Resume and Todo... » (visible) et cache les instructions dans le
    // champ « system » du prompt.
    let _lock = lock::acquire(dir)?;

    if sudo && !cfg!(unix) {
        return Err(anyhow!(
            "le mode --root (sudo) n'est pas supporté sur cette plateforme"
        ));
    }

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
        let cmd = format!(
          "cd {} && OPENCODE_AUTO_RESUME={} opencode --continue",
          shell_quote(&dir.to_string_lossy()),
          shell_quote(&prompt),
        );
        Command::new("sudo").args(["-i", "-c", &cmd]).status()
      }
    else
      {
        Command::new("opencode")
          .env("OPENCODE_AUTO_RESUME", &prompt)
          .arg("--continue")
          .current_dir(dir)
          .status()
      };

    match status
      {
        Ok(code) if code.success() => Ok(()),
        Ok(code) => Err(anyhow!(
          "opencode a échoué (code de sortie {:?})",
          code.code()
        )),
        Err(e) => Err(anyhow!("échec du lancement d'opencode : {e}")),
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