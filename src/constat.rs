use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Sous-répertoires jamais sondés : artefacts gérés ou caches, dont le
/// contenu n'est pas du travail éditorial. Leur présence (modifiée ou non)
/// ne doit ni fausser les deltas, ni alourdir la lecture du projet.
const DOSSIERS_IGNORES: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".venv",
    "__pycache__",
    "dist",
    "build",
    ".next",
    ".svelte-kit",
    ".DS_Store",
];

/// L'empreinte d'un fichier au moment du constat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EtatFichier
  {
    pub mtime_secs: u64,
    pub taille: u64,

    /// Nombre de lignes (`\n` + terminaison finale), lu au constat.
    pub lignes: u64,
  }

/// Constate l'état d'une arborescence : chemin relatif → empreinte.
///
/// Infaillible par construction : un fichier illisible est sauté, pas une
/// cause d'échec. Les liens symboliques ne sont jamais suivis (ni chargés
/// comme fichiers) : le chemin après un lien n'appartient pas à la racine.
pub(crate) fn constater(racine: &Path) -> BTreeMap<String, EtatFichier>
  {
    let mut etat = BTreeMap::new();
    if !racine.is_dir()
      {
        return etat;
      }
    let parcours = walkdir::WalkDir::new(racine)
      .follow_links(false)
      .into_iter()
      .filter_entry(|e| {
        e.depth() == 0
          || !e
            .file_name()
            .to_str()
            .is_some_and(|n| DOSSIERS_IGNORES.contains(&n))
      });
    for entree in parcours.flatten()
      {
        let ft = entree.file_type();
        if ft.is_dir() || !ft.is_file() || entree.path_is_symlink()
          {
            continue;
          }
        let Some(rel) = entree.path().strip_prefix(racine).ok() else {
          continue;
        };
        let meta = match std::fs::metadata(entree.path())
          {
            Ok(m) => m,
            Err(_) => continue,
          };
        let Ok(lus) = std::fs::read(entree.path()) else {
          continue;
        };
        let lignes = lus.iter().filter(|&&b| b == b'\n').count() as u64
            + u64::from(!lus.is_empty() && !lus.ends_with(b"\n"));
        etat.insert(
          rel.to_string_lossy().to_string(),
          EtatFichier {
            mtime_secs: meta
              .modified()
              .ok()
              .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
              .map(|d| d.as_secs())
              .unwrap_or(0),
            taille: meta.len(),
            lignes,
          },
        );
      }
    etat
  }

/// Delta net de lignes par fichier entre deux constats.
///
/// positif = lignes ajoutées, négatif = lignes retirées. Un fichier créé
/// entre les deux constats compte pour sa taille initiale, un fichier
/// supprimé pour son nombre de lignes d'avant (donc négatif).
pub(crate) fn diff_lignes(
  avant: &BTreeMap<String, EtatFichier>,
  apres: &BTreeMap<String, EtatFichier>,
) -> BTreeMap<String, i64>
  {
    let mut diff = BTreeMap::new();
    for (chemin, a) in avant
      {
        let avant_l = a.lignes as i64;
        let apres_l = apres
          .get(chemin)
          .map(|f| f.lignes as i64)
          .unwrap_or(0);
        if avant_l != apres_l
          {
            diff.insert(chemin.clone(), apres_l - avant_l);
          }
      }
    for (chemin, b) in apres
      {
        if !avant.contains_key(chemin)
          {
            diff.insert(chemin.clone(), b.lignes as i64);
          }
      }
    diff
  }

/// Bilan d'un diff : lignes ajoutées et supprimées (en valeur absolue) sur
/// l'ensemble de l'arbre.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BilanLignes
  {
    pub ajoute: u64,
    pub supprime: u64,
  }

/// Bilan agrégé d'un diff.
pub(crate) fn bilan(diff: &BTreeMap<String, i64>) -> BilanLignes
  {
    let mut res = BilanLignes::default();
    for &delta in diff.values()
      {
        if delta > 0
          {
            res.ajoute += delta as u64;
          }
        else
          {
            res.supprime += (-delta) as u64;
          }
      }
    res
  }

#[cfg(test)]
mod tests
  {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;

    use super::{bilan, constater, diff_lignes, BilanLignes};

    fn temp(nom: &str) -> PathBuf
      {
        let dir = std::env::temp_dir()
            .join(format!("op-constat-{}-{nom}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
      }

    #[test]
    fn un_arbre_vide_ne_constate_rien() {
        let dir = temp("vide");
        assert!(constater(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn le_constat_compte_les_lignes_et_ignore_les_caches() {
        let dir = temp("etat");
        fs::create_dir_all(dir.join("target")).unwrap();
        fs::write(dir.join("a.rs"), "ligne 1\nligne 2\n").unwrap();
        fs::write(dir.join("target/artefact"), "ignoré\n").unwrap();
        fs::write(dir.join("sans_eol"), "pas de retour").unwrap();
        let etat = constater(&dir);
        assert_eq!(etat.len(), 2);
        assert_eq!(etat["a.rs"].lignes, 2);
        assert_eq!(etat["sans_eol"].lignes, 1);
        assert!(!etat.contains_key("target/artefact"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn un_lien_symblique_n_est_jamais_suivi() {
        let dir = temp("lien");
        let cible = temp("cible");
        fs::write(cible.join("dedans.txt"), "x\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&cible, dir.join("pointe")).unwrap();
        let etat = constater(&dir);
        assert!(etat.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&cible);
    }

    #[test]
    fn la_diff_suit_ajout_modification_et_suppression() {
        let dir = temp("diff");
        fs::write(dir.join("stable"), "1\n").unwrap();
        fs::write(dir.join("modifie"), "a\nb\n").unwrap();
        let avant = constater(&dir);

        fs::write(dir.join("stable"), "1\n").unwrap();
        fs::write(dir.join("modifie"), "a\nb\nc\n").unwrap();
        fs::write(dir.join("nouveau"), "1\n2\n").unwrap();
        fs::remove_file(dir.join("stable")).unwrap();

        let apres = constater(&dir);
        let diff = diff_lignes(&avant, &apres);
        assert_eq!(diff.get("modifie"), Some(&1));
        assert_eq!(diff.get("nouveau"), Some(&2));
        assert_eq!(diff.get("stable").copied(), Some(-1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn le_bilan_comptabilise_ajouts_et_suppressions() {
        let mut diff: BTreeMap<String, i64> = BTreeMap::new();
        diff.insert("gagne".to_string(), 5);
        diff.insert("perdu".to_string(), -3);
        let b = bilan(&diff);
        assert_eq!(b.ajoute, 5);
        assert_eq!(b.supprime, 3);
        assert_eq!(b.ajoute + b.supprime, 8);
        assert_eq!(BilanLignes::default().ajoute, 0);
    }
  }