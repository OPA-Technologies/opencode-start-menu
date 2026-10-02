//! Régression P0-1 : le wrapper doit transmettre stdin à opencode.
//!
//! Le bug d'origine est le plus grave du projet : `Command::output()`
//! redirigeait stdin, si bien qu'opencode recevait un EOF immédiat et
//! quittait. Le TUI était inutilisable — opencode ne pouvait pas être lancé
//! depuis le menu. Aucun test unitaire ne pouvait le voir : le symptôme est
//! dans la manière dont le processus est démarré, pas dans une valeur
//! calculée. Il faut donc réellement lancer le binaire.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// Répertoire jetable, supprimé à la fin du test même en cas d'échec.
struct BacASable {
    chemin: std::path::PathBuf,
}

impl BacASable {
    fn nouveau(nom: &str) -> Self {
        let chemin = std::env::temp_dir().join(format!("opencode-wrap-test-{nom}-{}", std::process::id()));
        fs::remove_dir_all(&chemin).ok();
        fs::create_dir_all(&chemin).unwrap();
        Self { chemin }
    }

    fn chemin(&self) -> &Path {
        &self.chemin
    }
}

impl Drop for BacASable {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.chemin).ok();
    }
}

/// Emplacement du faux `opencode`, dans un sous-répertoire `bin/`.
///
/// Séparé de la racine du bac à sable : cette racine sert aussi de
/// `XDG_DATA_HOME`, et le wrapper y cherche `opencode/opencode.db`. Un binaire
/// nommé `opencode` à cet endroit empêcherait l'ouverture de la base, et le
/// test passerait sans jamais rien écrire.
fn faux_bin(repertoire: &Path) -> std::path::PathBuf {
    let bin = repertoire.join("bin");
    fs::create_dir_all(&bin).unwrap();
    bin.join("opencode")
}

fn ecrire_faux_opencode(repertoire: &Path, script: &str) -> std::path::PathBuf {
    let chemin = faux_bin(repertoire);
    fs::write(&chemin, script).unwrap();
    fs::set_permissions(&chemin, fs::Permissions::from_mode(0o755)).unwrap();
    chemin
}

/// Faux `opencode` : lit une ligne sur stdin et la restitue. S'il ne reçoit
/// rien, il affiche `AUCUNE_ENTREE`.
fn installer_faux_opencode(repertoire: &Path) {
    ecrire_faux_opencode(
        repertoire,
        r#"#!/bin/sh
if IFS= read -r ligne; then
  echo "recu:$ligne"
else
  echo "AUCUNE_ENTREE"
fi
"#,
    );
}

fn chemin_wrapper() -> std::path::PathBuf {
    // CARGO_BIN_EXE_<nom> est fourni par cargo pour les tests d'intégration.
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_opencode-wrap"))
}

fn chemin_bin(repertoire: &Path) -> String {
    let existant = std::env::var("PATH").unwrap_or_default();
    format!("{}:{existant}", faux_bin(repertoire).parent().unwrap().display())
}

/// Le test central : ce que l'utilisateur tape doit atteindre opencode.
#[test]
fn la_saisie_utilisateur_arrive_a_opencode() {
    let bac = BacASable::nouveau("stdin");
    installer_faux_opencode(bac.chemin());

    let mut enfant = Command::new(chemin_wrapper())
        .env("PATH", chemin_bin(bac.chemin()))
        // Isole la base de données : sans ce réglage, `dirs::data_local_dir()`
        // pointerait sur la vraie base opencode de l'utilisateur.
        .env("XDG_DATA_HOME", bac.chemin())
        .env_remove("OPENCODE_PROJECT_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("le wrapper doit demarrer");

    // Le cœur du test : une ligne est écrite sur stdin du wrapper.
    enfant
        .stdin
        .take()
        .unwrap()
        .write_all(b"bonjour opencode\n")
        .unwrap();

    let sortie = enfant.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&sortie.stdout);

    assert!(
        stdout.contains("recu:bonjour opencode"),
        "opencode n'a pas recu la saisie. stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&sortie.stderr)
    );
    assert!(
        !stdout.contains("AUCUNE_ENTREE"),
        "opencode a recu un stdin vide : l'heritage de stdin est casse"
    );
}

/// La capture ne doit pas avaler la sortie : opencode écrit sur stdout, et le
/// parent doit le lire sur sa propre sortie.
#[test]
fn la_sortie_est_restituee_sur_la_sortie_standard() {
    let bac = BacASable::nouveau("sortie");
    installer_faux_opencode(bac.chemin());

    let sortie = Command::new(chemin_wrapper())
        .env("PATH", chemin_bin(bac.chemin()))
        .env("XDG_DATA_HOME", bac.chemin())
        .env_remove("OPENCODE_PROJECT_DIR")
        .stdin(Stdio::null())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&sortie.stdout);
    assert!(
        stdout.contains("AUCUNE_ENTREE"),
        "la capture doit etre restituee telle quelle. stdout:\n{stdout}"
    );
}

/// OPENCODE_WRAP_CAPTURE=0 : opencode écrit directement dans le terminal. Le
/// contenu est le même, mais ce chemin ne dépend d'aucun fichier temporaire —
/// c'est l'échappatoire quand /tmp est inaccessible ou saturé.
#[test]
fn le_mode_sans_capture_reste_correct() {
    let bac = BacASable::nouveau("direct");
    installer_faux_opencode(bac.chemin());

    let mut enfant = Command::new(chemin_wrapper())
        .env("PATH", chemin_bin(bac.chemin()))
        .env("XDG_DATA_HOME", bac.chemin())
        .env("OPENCODE_WRAP_CAPTURE", "0")
        .env_remove("OPENCODE_PROJECT_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    enfant
        .stdin
        .take()
        .unwrap()
        .write_all(b"mode direct\n")
        .unwrap();

    let sortie = enfant.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&sortie.stdout);
    assert!(
        stdout.contains("recu:mode direct"),
        "le mode direct doit aussi transmettre stdin. stdout:\n{stdout}"
    );
}

/// P1-2 : quand le scraping ne trouve rien, la base ne doit pas être écrasée.
/// Ce test est la version « de bout en bout » du correctif : le faux opencode
/// n'affiche ni contexte ni coût, et le wrapper n'a rien à écrire.
#[test]
fn une_session_sans_info_n_ecrase_pas_les_donnees_precedentes() {
    use rusqlite::Connection;

    let bac = BacASable::nouveau("persistance");

    // Faux opencode silencieux sur les métriques : aucune ligne « context »,
    // aucune ligne « spent ».
    ecrire_faux_opencode(
        bac.chemin(),
        r#"#!/bin/sh
echo "session terminee"
"#,
    );

    // Base avec une session déjà renseignée. Le chemin doit être
    // `$XDG_DATA_HOME/opencode/opencode.db` : c'est là que le wrapper cherche.
    // Placé ailleurs, il ne trouverait rien, ne toucherait à rien, et le test
    // passerait sans rien vérifier.
    let data = bac.chemin().join("donnees");
    fs::create_dir_all(data.join("opencode")).unwrap();
    let db = data.join("opencode").join("opencode.db");
    {
        let c = Connection::open(&db).unwrap();
        c.execute_batch(
            "CREATE TABLE session (
                 id TEXT PRIMARY KEY, directory TEXT, time_created INTEGER,
                 context_pct INTEGER DEFAULT 0, currency TEXT DEFAULT 'USD',
                 exchange_rate REAL, cost_local REAL);
             INSERT INTO session VALUES ('s','/mon/projet',100,42,'EUR',1.0,7.5);",
        )
        .unwrap();
    }

    let statut = Command::new(chemin_wrapper())
        .env("PATH", chemin_bin(bac.chemin()))
        .env("XDG_DATA_HOME", &data)
        .env("OPENCODE_PROJECT_DIR", "/mon/projet")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(statut.status.success(), "le wrapper doit sortir en 0");

    let c = Connection::open(&db).unwrap();
    let (pct, devise, cout): (i64, String, Option<f64>) = c
        .query_row(
            "SELECT context_pct, currency, cost_local FROM session WHERE id = 's'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (pct, devise.as_str(), cout),
        (42, "EUR", Some(7.5)),
        "une session sans métriques ne doit pas effacer les données réelles"
    );
}

/// Le code de sortie d'opencode doit être celui du wrapper : c'est ce qui
/// permet aux scripts et à opencode lui-même de détecter un échec.
#[test]
fn le_code_de_sortie_est_propage() {
    let bac = BacASable::nouveau("code");
    ecrire_faux_opencode(bac.chemin(), "#!/bin/sh\necho \"echec\" >&2\nexit 7\n");

    let sortie = Command::new(chemin_wrapper())
        .env("PATH", chemin_bin(bac.chemin()))
        .env("XDG_DATA_HOME", bac.chemin())
        .env_remove("OPENCODE_PROJECT_DIR")
        .stdin(Stdio::null())
        .output()
        .unwrap();

    assert_eq!(sortie.status.code(), Some(7));

    // opencode tourne dans un pseudo-terminal (sinon il n'écrit rien, cf. P0-2),
    // et un PTY fusionne stdout et stderr en un seul flux : le message écrit
    // par l'enfant sur sa stderr ressort donc sur notre stdout. Ce qui compte
    // ici — et qui est vérifié — c'est qu'aucun diagnostic ne soit perdu.
    let stdout = String::from_utf8_lossy(&sortie.stdout);
    let stderr = String::from_utf8_lossy(&sortie.stderr);
    assert!(
        stdout.contains("echec") || stderr.contains("echec"),
        "le diagnostic de l'enfant doit etre restitue, stdout={stdout:?} stderr={stderr:?}"
    );
}

/// Le chemin des arguments doit être transmis tel quel.
#[test]
fn les_arguments_sont_transmis() {
    let bac = BacASable::nouveau("args");
    ecrire_faux_opencode(bac.chemin(), "#!/bin/sh\necho \"args:$*\"\n");

    let sortie = Command::new(chemin_wrapper())
        .arg("run")
        .arg("--modele")
        .arg("anthropic/claude")
        .env("PATH", chemin_bin(bac.chemin()))
        .env("XDG_DATA_HOME", bac.chemin())
        .env_remove("OPENCODE_PROJECT_DIR")
        .stdin(Stdio::null())
        .output()
        .unwrap();

    assert!(
        String::from_utf8_lossy(&sortie.stdout).contains("args:run --modele anthropic/claude"),
        "les arguments doivent etre transmis intacts"
    );
}

/// Régression P0-2 : le pourcentage de contexte doit être extrait de la
/// sortie d'opencode et écrit en base.
///
/// Le mécanisme de démarrage est la cause : la capture redirigeait stdout vers
/// un fichier régulier, or opencode n'écrit rien dans ce cas — il détecte un
/// stdout non terminal et se taît. La capture restait donc vide et
/// `context_pct` valait 0 sur toutes les sessions. Aucun test unitaire ne
/// pouvait le voir : il faut lancer le binaire pour que le pseudo-terminal
/// existe.
///
/// Le faux opencode produit une sortie de TUI réaliste (séquences ANSI et retour
/// chariot), et non une ligne propre : c'est ce que reçoit réellement le
/// pseudo-terminal.
/// Le faux opencode doit reproduire le comportement mesuré d'opencode : il
/// n'écrit rien si sa sortie n'est pas un terminal. C'est toute la cause du
/// bug ; un faux qui écrit inconditionnellement passerait aussi avec le code
/// d'avant la correction, et ne prouverait donc rien.
#[test]
fn la_capture_pty_ecrit_le_contexte_en_base() {
    use rusqlite::Connection;

    let bac = BacASable::nouveau("capture");
    ecrire_faux_opencode(
        bac.chemin(),
        r#"#!/bin/sh
[ -t 1 ] || exit 0
printf '\033[2K\r'
printf '  Context: 42%% used  \r\n'
printf '\033[0m\033[38;5;33mau revoir\033[0m\n'
"#,
    );

    let data = bac.chemin().join("donnees");
    fs::create_dir_all(data.join("opencode")).unwrap();
    let db = data.join("opencode").join("opencode.db");
    {
        let c = Connection::open(&db).unwrap();
        c.execute_batch(
            "CREATE TABLE session (
                 id TEXT PRIMARY KEY, directory TEXT, time_created INTEGER,
                 context_pct INTEGER DEFAULT 0);
             INSERT INTO session VALUES ('s','/mon/projet',100,0);",
        )
        .unwrap();
    }

    let statut = Command::new(chemin_wrapper())
        .env("PATH", chemin_bin(bac.chemin()))
        .env("XDG_DATA_HOME", &data)
        .env("OPENCODE_PROJECT_DIR", "/mon/projet")
        .stdin(Stdio::null())
        .status()
        .unwrap();
    assert!(statut.success(), "le wrapper doit sortir en 0");

    let c = Connection::open(&db).unwrap();
    let pct: i64 = c
        .query_row("SELECT context_pct FROM session WHERE id = 's'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(pct, 42, "le contexte du TUI doit etre extrait puis ecrit");
}
