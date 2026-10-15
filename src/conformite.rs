//! Conformité du code d'un projet au contrat `-V`.
//!
//! Un projet qui contient du code doit déclarer, dans son **module principal**,
//! trois constantes figées à la compilation : `RELEASE` (`x.y.z`), `STATUT`
//! (valeur de la liste connue d'op) et `DATE_LAST_MAJ` (`jj-mm-yyyy`). Sa sortie
//! `-V` publie alors `<version>_<statut> (<jj-mm-yyyy>)`, seule vérité que op
//! consulte.
//!
//! Op ne réécrit jamais tout le projet : il ne touche qu'au module principal, et
//! seulement à ces trois déclarations. Les bibliothèques (un projet qui n'a que
//! `src/lib.rs`, sans binaire) sont exclues : elles ne publient pas de `-V`.
//!
//! Le module est volontairement pur : détection du module et réécriture des
//! constantes se testent sans disque ni compilation.

use std::path::{Path, PathBuf};

/// Localise le module principal d'un projet Rust.
///
/// Priorité : `src/main.rs`, puis le `path` du premier `[[bin]]` du manifeste.
/// Un projet sans binaire (bibliothèque seule) n'a pas de `-V` : `None`.
pub(crate) fn module_principal(dir: &Path) -> Option<PathBuf>
  {
    let main = dir.join("src/main.rs");
    if main.is_file()
      {
        return Some(main);
      }
    if let Some(bin) = chemin_premier_bin(dir)
        && bin.is_file()
      {
        return Some(bin);
      }
    None
  }

/// `path` du premier `[[bin]]` du manifeste, résolu depuis `[package] name`.
///
/// Un `[[bin]]` sans `path` prend le chemin par défaut `src/bin/<name>.rs`.
fn chemin_premier_bin(dir: &Path) -> Option<PathBuf>
  {
    let cargo = std::fs::read_to_string(dir.join("Cargo.toml")).ok()?;
    let mut dans_bin = false;
    let mut nom: Option<String> = None;
    let mut chemin: Option<String> = None;
    for ligne in cargo.lines()
      {
        let t = ligne.trim();
        if t.starts_with('[')
          {
            // La section change : un [[bin]] complet a été vu.
            if dans_bin
              {
                break;
              }
            dans_bin = t == "[[bin]]";
            continue;
          }
        if !dans_bin
          {
            continue;
          }
        if let Some((cle, valeur)) = t.split_once('=')
          {
            let cle = cle.trim();
            let v = valeur.trim().trim_matches('"').to_string();
            match cle
              {
                "name" => nom = Some(v),
                "path" => chemin = Some(v),
                _ => {}
              }
          }
      }
    if let Some(c) = chemin
      {
        return Some(dir.join(c));
      }
    nom.map(|n| dir.join("src/bin").join(format!("{n}.rs")))
  }

/// Conforme un module : s'assure que les trois constantes existent et portent
/// les valeurs voulues. Renvoie le nouveau contenu et si le fichier change.
pub(crate) fn conformer_constantes(
    contenu: &str,
    version: &str,
    statut: &str,
    date: &str,
) -> (String, bool)
  {
    let mut out = contenu.to_string();
    let mut modifie = false;
    let mut manquantes: Vec<(&str, &str)> = Vec::new();
    for (nom, valeur) in [
        ("RELEASE", version),
        ("STATUT", statut),
        ("DATE_LAST_MAJ", date),
      ]
      {
        if contient_constante(&out, nom)
          {
            let (n, m) = maj_constante(&out, nom, valeur);
            out = n;
            modifie |= m;
          }
        else
          {
            manquantes.push((nom, valeur));
          }
      }
    if !manquantes.is_empty()
      {
        out = inserer_constantes(&out, &manquantes);
        modifie = true;
      }
    (out, modifie)
  }

/// La constante `nom` est-elle déclarée dans le contenu ?
fn contient_constante(contenu: &str, nom: &str) -> bool
  {
    contenu.lines().any(|l| ligne_declare(l, nom))
  }

/// Une ligne déclare-t-elle `const <nom>` (frontière de mot respectée) ?
fn ligne_declare(ligne: &str, nom: &str) -> bool
  {
    let t = ligne.trim_start();
    let Some(reste) = t.strip_prefix("const")
      else
        {
          return false;
        };
    if !reste.starts_with(char::is_whitespace)
      {
        return false;
      }
    let apres = reste.trim_start();
    let Some(queue) = apres.strip_prefix(nom)
      else
        {
          return false;
        };
    // Frontière : le nom n'est pas le préfixe d'un identifiant plus long.
    match queue.chars().next()
      {
        Some(c) => !(c.is_alphanumeric() || c == '_'),
        None => true,
      }
  }

/// Réécrit la valeur littérale de `const <nom>: &str = "..."`.
///
/// Seule une valeur **littérale** (dont l'expression commence par un guillemet)
/// est touchée : une valeur calculée comme `env!("CARGO_PKG_VERSION")` est
/// laissée intacte, car elle porte la vérité de compilation que le contrat
/// recherche. Renvoie le contenu et s'il a changé.
fn maj_constante(contenu: &str, nom: &str, valeur: &str) -> (String, bool)
  {
    let mut out = String::with_capacity(contenu.len());
    let mut modifie = false;
    let mut fait = false;
    for part in contenu.split_inclusive('\n')
      {
        if !fait && ligne_declare(part, nom)
            && let Some(nouvelle) = remplacer_litteral(part, valeur)
          {
            if nouvelle != part
              {
                modifie = true;
              }
            out.push_str(&nouvelle);
            fait = true;
            continue;
          }
        out.push_str(part);
      }
    (out, modifie)
  }

/// Remplace le contenu du premier littéral `"..."` de la ligne, si l'expression
/// après `=` est bien un littéral. Renvoie `None` sinon.
fn remplacer_litteral(ligne: &str, valeur: &str) -> Option<String>
  {
    let eq = ligne.find('=')?;
    let (avant, apres) = ligne.split_at(eq + 1);
    let (espaces, reste) = decouper_espaces(apres);
    if !reste.starts_with('"')
      {
        return None;
      }
    let apres_q = &reste[1..];
    let fin = fin_litteral(apres_q)?;
    let queue = &apres_q[fin + 1..];
    Some(format!("{avant}{espaces}\"{valeur}\"{queue}"))
  }

/// Sépare les espaces de tête du reste de la chaîne.
fn decouper_espaces(texte: &str) -> (&str, &str)
  {
    let coupe = texte.len() - texte.trim_start().len();
    texte.split_at(coupe)
  }

/// Index de fermeture d'un littéral, en tolérant les guillemets échappés.
fn fin_litteral(apres_ouvrant: &str) -> Option<usize>
  {
    let mut echappe = false;
    for (i, c) in apres_ouvrant.char_indices()
      {
        if echappe
          {
            echappe = false;
            continue;
          }
        match c
          {
            '\\' => echappe = true,
            '"' => return Some(i),
            _ => {}
          }
      }
    None
  }

/// Insère les constantes manquantes après les attributs internes et la doc de
/// module, pour ne pas séparer un `#![...]` de son fichier.
fn inserer_constantes(contenu: &str, manquantes: &[(&str, &str)]) -> String
  {
    let mut insertion = String::new();
    for (nom, valeur) in manquantes
      {
        insertion.push_str(&format!("const {nom}: &str = \"{valeur}\";\n"));
      }
    let mut offset = 0usize;
    for part in contenu.split_inclusive('\n')
      {
        let t = part.trim_start();
        if t.is_empty() || t.starts_with("//!") || t.starts_with("#![")
          {
            offset += part.len();
          }
        else
          {
            break;
          }
      }
    let mut out = String::with_capacity(contenu.len() + insertion.len() + 1);
    out.push_str(&contenu[..offset]);
    out.push_str(&insertion);
    out.push_str(&contenu[offset..]);
    out
  }

#[cfg(test)]
mod tests
  {
    use super::*;
    use std::fs;

    fn temp_dir(nom: &str) -> PathBuf
      {
        let dir = std::env::temp_dir().join(format!(
          "op-conformite-{}-{nom}",
          std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
      }

    #[test]
    fn un_fichier_sans_constantes_les_insere()
      {
        let src = "#![deny(warnings)]\n//! Doc.\n\nfn main() {}\n";
        let (out, modifie) = conformer_constantes(src, "1.2.3", "Release", "15-10-2026");
        assert!(modifie);
        assert!(out.contains("const RELEASE: &str = \"1.2.3\";"));
        assert!(out.contains("const STATUT: &str = \"Release\";"));
        assert!(out.contains("const DATE_LAST_MAJ: &str = \"15-10-2026\";"));
        // L'insertion suit la doc et l'attribut, sans les séparer du fichier.
        let pos_doc = out.find("//! Doc.").unwrap();
        let pos_const = out.find("const RELEASE").unwrap();
        assert!(pos_doc < pos_const, "les constantes viennent après la doc");
      }

    #[test]
    fn une_valeur_litterale_est_mise_a_jour()
      {
        let src = "const RELEASE: &str = \"0.1.0\";\n";
        let (out, modifie) = conformer_constantes(src, "0.2.0", "Release", "15-10-2026");
        assert!(modifie);
        assert!(out.contains("const RELEASE: &str = \"0.2.0\";"));
        // Les deux autres sont insérées, la valeur littérale seule est réécrite.
        assert!(out.contains("const STATUT: &str = \"Release\";"));
      }

    #[test]
    fn une_valeur_calculee_n_est_pas_touchee()
      {
        let src = "const RELEASE: &str = env!(\"CARGO_PKG_VERSION\");\n";
        let (out, _) = conformer_constantes(src, "9.9.9", "Release", "15-10-2026");
        assert!(
          out.contains("env!(\"CARGO_PKG_VERSION\")"),
          "une valeur de compilation fait foi : {out}"
        );
        assert!(!out.contains("9.9.9"));
      }

    #[test]
    fn un_nom_plus_long_n_est_pas_confondu()
      {
        let src = "const RELEASE_NOTES: &str = \"x\";\n";
        assert!(!contient_constante(src, "RELEASE"));
        let (out, _) = conformer_constantes(src, "1.0.0", "Release", "15-10-2026");
        assert!(out.contains("RELEASE_NOTES"));
        assert!(out.contains("const RELEASE: &str = \"1.0.0\";"));
      }

    #[test]
    fn un_contenu_deja_conforme_n_est_pas_modifie()
      {
        let src = "const RELEASE: &str = \"1.0.0\";\n\
                   const STATUT: &str = \"Release\";\n\
                   const DATE_LAST_MAJ: &str = \"15-10-2026\";\n";
        let (out, modifie) = conformer_constantes(src, "1.0.0", "Release", "15-10-2026");
        assert!(!modifie, "rien à changer : {out}");
        assert_eq!(out, src);
      }

    #[test]
    fn le_guillemet_echappe_ne_tronque_pas_la_valeur()
      {
        let src = "const STATUT: &str = \"A\\\"B\"; // note\n";
        let (out, modifie) = conformer_constantes(src, "1.0.0", "Release", "15-10-2026");
        assert!(modifie);
        assert!(out.contains("const STATUT: &str = \"Release\"; // note"));
      }

    #[test]
    fn le_module_principal_est_main_rs_quand_il_existe()
      {
        let dir = temp_dir("main");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        assert_eq!(module_principal(&dir), Some(dir.join("src/main.rs")));
      }

    #[test]
    fn une_bibliotheque_seule_n_a_pas_de_module_principal()
      {
        let dir = temp_dir("lib");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
        assert_eq!(module_principal(&dir), None);
      }

    #[test]
    fn le_chemin_du_premier_bin_du_manifeste_est_utilise()
      {
        let dir = temp_dir("bin");
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"1.0.0\"\n\n\
             [[bin]]\nname = \"outil\"\npath = \"bin/outil.rs\"\n",
        )
        .unwrap();
        fs::write(dir.join("bin/outil.rs"), "fn main() {}\n").unwrap();
        assert_eq!(module_principal(&dir), Some(dir.join("bin/outil.rs")));
      }
  }
