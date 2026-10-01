use anyhow::Result;
use dirs;
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::Client;
use rusqlite::Connection;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use tokio::process::Command;

static HTTP_CLIENT: Lazy<Client> = Lazy::new(|| Client::new());
static EXCHANGE_RATE_CACHE: Lazy<Mutex<HashMap<String, f64>>> = Lazy::new(|| Mutex::new(HashMap::new()));

/// Flux du parent sur lequel une capture est restituée.
#[derive(Clone, Copy)]
enum Flux {
    Stdout,
    Stderr,
}

/// Capture sur fichier temporaire de la sortie d'opencode.
///
/// `Command::output()` capturait déjà stdout et stderr, mais sans stdin :
/// opencode recevait un EOF immédiat et quittait sans laisser l'utilisateur
/// taper. Il faut donc laisser stdin au terminal tout en conservant une copie
/// de la sortie pour le scraping, ce qu'`output()` ne permet pas.
///
/// La restitution se fait après la sortie de l'enfant, en une seule fois :
/// c'est la contrepartie assumée du correctif minimal, le rendu en direct n'est
/// plus possible (voir `OPENCODE_WRAP_CAPTURE`).
struct CaptureTemp {
    path: PathBuf,
}

impl CaptureTemp {
    /// Crée le fichier de capture et renvoie le descripteur à passer à
    /// `Stdio::from`. Le nom combine le PID et l'horodatage pour que deux
    /// sessions simultanées, ou deux captures d'un même processus, ne se
    /// marchent pas dessus.
    fn nouveau(etiquette: &str) -> std::io::Result<(Self, std::fs::File)> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "opencode-wrap-{etiquette}-{}-{nanos}",
            std::process::id()
        ));
        let fd = std::fs::File::create(&path)?;
        Ok((
            Self { path },
            fd,
        ))
    }

    /// Restitue la sortie capturée sur le flux du parent, supprime le fichier
    /// temporaire et rend les octets capturés pour le scraping.
    ///
    /// Une erreur de lecture ou d'écriture est ignorée : opencode s'est déjà
    /// terminé et la session est sauvée, seul l'affichage est en jeu.
    fn restituer(&self, flux: Flux) -> Vec<u8> {
        let contenu = std::fs::read(&self.path).unwrap_or_default();
        let mut sortie: Box<dyn Write> = match flux {
            Flux::Stdout => Box::new(std::io::stdout()),
            Flux::Stderr => Box::new(std::io::stderr()),
        };
        if !contenu.is_empty() {
            let _ = sortie.write_all(&contenu);
            let _ = sortie.flush();
        }
        let _ = std::fs::remove_file(&self.path);
        contenu
    }
}

/// Prépare la capture d'un flux. Retourne `None` si le fichier temporaire n'a
/// pas pu être créé : on préfère alors la sortie en direct, dégradée mais
/// lisible, à une session silencieuse.
fn preparer_capture(etiquette: &str) -> Option<(CaptureTemp, Stdio)> {
    match CaptureTemp::nouveau(etiquette) {
        Ok((cap, fd)) => Some((cap, Stdio::from(fd))),
        Err(e) => {
            eprintln!("opencode-wrap: capture impossible ({e}), sortie en direct");
            None
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let project_dir = std::env::var("OPENCODE_PROJECT_DIR").ok();

    let mut cmd = Command::new("opencode");
    cmd.args(&args);

    // Régression P0-1 : sans cet héritage explicite, `output()` redirigeait
    // stdin et opencode recevait un EOF immédiat — le TUI était inutilisable.
    cmd.stdin(Stdio::inherit());

    // OPENCODE_WRAP_CAPTURE=0 : opencode écrit directement dans notre terminal.
    // Le rendu est alors en direct, mais plus rien n'est scrapé, donc ni
    // contexte ni coût ne sont enregistrés. À réserver au débogage.
    let capturer = std::env::var("OPENCODE_WRAP_CAPTURE").as_deref() != Ok("0");

    let mut cap_out = None;
    let mut cap_err = None;
    if capturer {
        if let Some((cap, fd)) = preparer_capture("out") {
            cap_out = Some(cap);
            cmd.stdout(fd);
        }
        if let Some((cap, fd)) = preparer_capture("err") {
            cap_err = Some(cap);
            cmd.stderr(fd);
        }
    }

    // `status()` et non `output()` : on veut laisser le terminal à opencode.
    let status = cmd.status().await?;
    let exit_code = status.code().unwrap_or(1);

    // Restitution avant le scraping, pour que l'utilisateur n'attende pas le
    // réseau de la conversion de devise.
    let stdout_cap = cap_out
        .as_ref()
        .map(|c| c.restituer(Flux::Stdout))
        .unwrap_or_default();
    let stderr_cap = cap_err
        .as_ref()
        .map(|c| c.restituer(Flux::Stderr))
        .unwrap_or_default();

    if let Some(ref dir) = project_dir {
        if let Err(e) = update_session_data(dir, &stdout_cap, &stderr_cap).await {
            eprintln!("opencode-wrap: erreur mise à jour session: {e}");
        }
    }

    std::process::exit(exit_code);
}

async fn update_session_data(project_dir: &str, stdout: &[u8], stderr: &[u8]) -> Result<()> {
    let context_pct = extract_context_pct(stdout, stderr);
    let (currency, cost_original) = extract_cost_and_currency(stdout, stderr);

    let db_path = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("opencode/opencode.db");

    if !db_path.exists() {
        return Ok(());
    }

    let conn = Connection::open(&db_path)?;

    let exchange_rate = match currency {
        Some(ref curr) => fetch_exchange_rate(curr, "EUR").await,
        None => None,
    };

    let cost_local = match (exchange_rate, cost_original) {
        (Some(rate), Some(cost)) => Some(cost * rate),
        _ => None,
    };

    persist_session(
        &conn,
        project_dir,
        context_pct,
        currency.as_deref(),
        exchange_rate,
        cost_local,
    )?;

    Ok(())
}

/// Applique les migrations de colonnes puis reporte contexte, devise et
/// coûts sur la session la plus récente du répertoire.
///
/// Les champs à écrire sont des `Option` : voir `maj_session` pour pourquoi un
/// champ non extrait ne doit pas écraser la valeur précédente.
///
/// Les `ALTER TABLE ADD COLUMN` sont volontairement ignorés : les colonnes
/// existent déjà après la première exécution (et le menu les crée de son
/// côté), donc SQLite renvoie « duplicate column ». Propager cette erreur
/// faisait sortir la fonction avant l'écriture — le wrapper n'écrivait donc
/// jamais rien, silencieusement.
fn persist_session(
    conn: &Connection,
    project_dir: &str,
    context_pct: Option<u8>,
    currency: Option<&str>,
    exchange_rate: Option<f64>,
    cost_local: Option<f64>,
) -> rusqlite::Result<usize> {
    let _ = conn.execute(
        "ALTER TABLE session ADD COLUMN currency TEXT DEFAULT 'USD'",
        [],
    );
    let _ = conn.execute("ALTER TABLE session ADD COLUMN exchange_rate REAL", []);
    let _ = conn.execute("ALTER TABLE session ADD COLUMN cost_local REAL", []);
    // Le wrapper doit tenir seul : sur une installation fraîche où le menu
    // n'a jamais tourné, context_pct n'existe pas encore et l'écriture
    // échouerait sur « no such column ».
    let _ = conn.execute(
        "ALTER TABLE session ADD COLUMN context_pct INTEGER DEFAULT 0",
        [],
    );

    // Cible : la session la plus récente du répertoire. La sélection passe par
    // une sous-requête sur `id` car `ORDER BY … LIMIT` n'existe pas dans un
    // UPDATE pour le SQLite embarqué de rusqlite (« near "ORDER": syntax
    // error ») : la requête échouait et aucune donnée n'était jamais écrite.
    maj_session(conn, project_dir, context_pct, currency, exchange_rate, cost_local)
}

/// Reporte contexte, devise et coûts sur la session la plus récente du
/// répertoire donné. Retourne le nombre de lignes mises à jour (0 si aucune
/// session ne correspond, ou si rien n'a été extrait), pour que l'appelant
/// puisse distinguer « rien à faire » d'une écriture effective.
///
/// Régression P1-2 : la requête écrivait les quatre colonnes inconditionnellement.
/// Quand l'extraction échouait — opencode interrompu, sortie sans ligne de
/// coût, TUI sans résumé — le wrapper remettait `context_pct` à 0, la devise à
/// « USD » et `cost_local` à NULL. Une session correcte était donc effacée par une
/// session qui ne l'était pas. La clause `SET` est désormais construite sur les
/// seuls champs réellement extraits ; un champ absent conserve sa valeur.
///
/// Contrainte de nommage : les noms de colonnes viennent d'une liste interne
/// constants, jamais d'une entrée utilisateur, donc l'interpolation est sûre.
fn maj_session(
    conn: &Connection,
    project_dir: &str,
    context_pct: Option<u8>,
    currency: Option<&str>,
    exchange_rate: Option<f64>,
    cost_local: Option<f64>,
) -> rusqlite::Result<usize> {
    use rusqlite::types::Value;

    let mut colonnes: Vec<&str> = Vec::new();
    let mut valeurs: Vec<Value> = Vec::new();

    if let Some(pct) = context_pct {
        colonnes.push("context_pct");
        valeurs.push(Value::Integer(pct as i64));
    }
    if let Some(curr) = currency {
        colonnes.push("currency");
        valeurs.push(Value::Text(curr.to_string()));
    }
    if let Some(rate) = exchange_rate {
        colonnes.push("exchange_rate");
        valeurs.push(Value::Real(rate));
    }
    if let Some(cost) = cost_local {
        colonnes.push("cost_local");
        valeurs.push(Value::Real(cost));
    }

    // Rien n'a été extrait : ne lance pas un UPDATE qui n'écrirait que des
    // valeurs par défaut, ce qui était précisément le bug.
    if colonnes.is_empty() {
        return Ok(0);
    }

    let set = colonnes
        .iter()
        .map(|c| format!("{c} = ?"))
        .collect::<Vec<_>>()
        .join(", ");

    // Cible : la session la plus récente du répertoire. La sélection passe par
    // une sous-requête sur `id` car `ORDER BY … LIMIT` n'existe pas dans un
    // UPDATE pour le SQLite embarqué de rusqlite (« near "ORDER": syntax
    // error ») : la requête échouait et aucune donnée n'était jamais écrite.
    let sql = format!(
        "UPDATE session SET {set} \
         WHERE id = (SELECT id FROM session WHERE directory = ? ORDER BY time_created DESC LIMIT 1)"
    );
    valeurs.push(Value::Text(project_dir.to_string()));
    conn.execute(&sql, rusqlite::params_from_iter(valeurs.iter()))
}

/// Extrait le pourcentage de contexte.
///
/// Régression P1-2 : le retour était un `u8` dont le « rien trouvé » valait
/// `0`, indiscernable d'un contexte réellement vide. Combiné à l'écriture
/// inconditionnelle de `maj_session`, une session dont la sortie ne mentionne
/// pas le contexte remettait le champ à 0. Le retour est donc un `Option<u8>`,
/// et `None` ne touche pas la base.
fn extract_context_pct(stdout: &[u8], stderr: &[u8]) -> Option<u8> {
    let stdout_str = String::from_utf8_lossy(stdout);
    let stderr_str = String::from_utf8_lossy(stderr);
    let combined = format!("{}\n{}", stdout_str, stderr_str);

    let patterns = [
        r"(?i)context\s*[:=]\s*(\d{1,3})\s*%",
        r"(?i)(\d{1,3})\s*%\s*context",
        r"(?i)context\s*(\d{1,3})\s*%",
        r"(\d{1,3})\s*%\s*used",
    ];

    for pattern in &patterns {
        if let Ok(re) = Regex::new(pattern) {
            if let Some(caps) = re.captures(&combined) {
                if let Some(m) = caps.get(1) {
                    if let Ok(pct) = m.as_str().parse::<u8>() {
                        return Some(pct.min(100));
                    }
                }
            }
        }
    }

    for line in combined.lines().rev().take(50) {
        let line_lower = line.to_lowercase();
        if line_lower.contains("context") && line_lower.contains('%') {
            let re = Regex::new(r"(\d{1,3})\s*%").unwrap();
            if let Some(caps) = re.captures(line) {
                if let Ok(pct) = caps.get(1).unwrap().as_str().parse::<u8>() {
                    return Some(pct.min(100));
                }
            }
        }
    }

    None
}

fn extract_cost_and_currency(stdout: &[u8], stderr: &[u8]) -> (Option<String>, Option<f64>) {
    let stdout_str = String::from_utf8_lossy(stdout);
    let stderr_str = String::from_utf8_lossy(stderr);
    let combined = format!("{}\n{}", stdout_str, stderr_str);

    let cost_re = Regex::new(r"(?i)spent\s+([$€£¥])\s*([\d,]+\.?\d*)").unwrap();
    if let Some(caps) = cost_re.captures(&combined) {
        let symbol = caps.get(1).map(|m| m.as_str()).unwrap_or("$");
        let amount_str = caps.get(2).map(|m| m.as_str().replace(',', "")).unwrap_or_default();
        if let Ok(amount) = amount_str.parse::<f64>() {
            let currency = match symbol {
                "$" => "USD",
                "€" => "EUR",
                "£" => "GBP",
                "¥" => "JPY",
                _ => "USD",
            };
            return (Some(currency.to_string()), Some(amount));
        }
    }

    let cost_re2 = Regex::new(r"(?i)spent\s+([\d,]+\.?\d*)\s*([$€£¥])").unwrap();
    if let Some(caps) = cost_re2.captures(&combined) {
        let amount_str = caps.get(1).map(|m| m.as_str().replace(',', "")).unwrap_or_default();
        let symbol = caps.get(2).map(|m| m.as_str()).unwrap_or("$");
        if let Ok(amount) = amount_str.parse::<f64>() {
            let currency = match symbol {
                "$" => "USD",
                "€" => "EUR",
                "£" => "GBP",
                "¥" => "JPY",
                _ => "USD",
            };
            return (Some(currency.to_string()), Some(amount));
        }
    }

    (None, None)
}

async fn fetch_exchange_rate(from: &str, to: &str) -> Option<f64> {
    let cache_key = format!("{from}->{to}");
    {
        let cache = EXCHANGE_RATE_CACHE.lock().unwrap();
        if let Some(&rate) = cache.get(&cache_key) {
            return Some(rate);
        }
    }

    let url = format!("https://api.exchangerate.host/latest?base={from}&symbols={to}");
    let response = HTTP_CLIENT.get(&url).send().await.ok()?;
    let json: serde_json::Value = response.json().await.ok()?;
    let rate = json.get("rates")?.get(to)?.as_f64()?;

    if rate > 0.0 {
        let mut cache = EXCHANGE_RATE_CACHE.lock().unwrap();
        cache.insert(cache_key, rate);
        Some(rate)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn extract_context_pct_basic() {
        assert_eq!(extract_context_pct(b"Context: 50%", b""), Some(50));
        assert_eq!(extract_context_pct(b"context: 75%", b""), Some(75));
        assert_eq!(extract_context_pct(b"50% context", b""), Some(50));
    }

    /// Régression P1-2 : sans mention de contexte, l'ancien code renvoyait
    /// `Some(0)`-équivalent, indiscernable d'une session à 0 % réel.
    #[test]
    fn extract_context_pct_absent_ne_vaut_pas_zero() {
        assert_eq!(extract_context_pct(b"session terminee", b""), None);
        assert_eq!(extract_context_pct(b"", b""), None);
        assert_eq!(extract_context_pct(b"erreur fatale", b"aucune donnee"), None);
        // Une progression sans pourcentage n'est pas un contexte.
        assert_eq!(extract_context_pct(b"context: en cours", b""), None);
        // Un vrai 0 % reste un 0 mesuré, pas une absence.
        assert_eq!(extract_context_pct(b"Context: 0%", b""), Some(0));
    }

    #[test]
    fn extract_cost_and_currency_usd() {
        let out = b"spent $45.67";
        let (curr, cost) = extract_cost_and_currency(out, b"");
        assert_eq!(curr, Some("USD".to_string()));
        assert_eq!(cost, Some(45.67));
    }

    #[test]
    fn extract_cost_and_currency_eur() {
        let out = "spent €38.50".as_bytes();
        let (curr, cost) = extract_cost_and_currency(out, b"");
        assert_eq!(curr, Some("EUR".to_string()));
        assert_eq!(cost, Some(38.50));
    }

    #[test]
    fn extract_cost_and_currency_gbp() {
        let out = "spent £12.00".as_bytes();
        let (curr, cost) = extract_cost_and_currency(out, b"");
        assert_eq!(curr, Some("GBP".to_string()));
        assert_eq!(cost, Some(12.00));
    }

    #[test]
    fn extract_cost_and_currency_jpy() {
        let out = "spent ¥1500".as_bytes();
        let (curr, cost) = extract_cost_and_currency(out, b"");
        assert_eq!(curr, Some("JPY".to_string()));
        assert_eq!(cost, Some(1500.0));
    }

    #[test]
    fn maj_session_ecrit_reellement_sur_la_session_la_plus_recente() {
        // Régression : la requête utilisait `ORDER BY … LIMIT` directement
        // dans un UPDATE, ce que le SQLite embarqué de rusqlite refuse
        // ("near \"ORDER\": syntax error"). L'écriture échouait donc en
        // silence et aucune donnée n'était jamais stockée. Ce test vérifie
        // l'écriture réelle, pas seulement la syntaxe.
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE session (
                 id TEXT PRIMARY KEY,
                 directory TEXT,
                 time_created INTEGER,
                 context_pct INTEGER DEFAULT 0,
                 currency TEXT DEFAULT 'USD',
                 exchange_rate REAL,
                 cost_local REAL
             );
             INSERT INTO session VALUES ('vieille','/p',100,0,'USD',NULL,NULL);
             INSERT INTO session VALUES ('recente','/p',200,0,'USD',NULL,NULL);
             INSERT INTO session VALUES ('autre','/autre',300,0,'USD',NULL,NULL);",
        )
        .unwrap();

        let n = maj_session(&c, "/p", Some(42), Some("EUR"), Some(0.92), Some(9.2)).unwrap();
        assert_eq!(n, 1, "une seule session doit etre mise a jour");

        let lire = |id: &str| -> (i64, String, Option<f64>, Option<f64>) {
            c.query_row(
                "SELECT context_pct, currency, exchange_rate, cost_local FROM session WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
        };

        // La plus récente du répertoire, et elle seule.
        assert_eq!(lire("recente"), (42, "EUR".to_string(), Some(0.92), Some(9.2)));
        assert_eq!(lire("vieille").1, "USD".to_string());
        assert_eq!(lire("autre").1, "USD".to_string(), "un autre répertoire ne doit pas etre touche");
    }

    #[test]
    fn persist_session_ecrit_meme_si_les_colonnes_existent_deja() {
        // Régression du blocage principal : les colonnes sont déjà présentes
        // (créées par le menu ou par une exécution précédente), donc chaque
        // « ALTER TABLE ADD COLUMN » échoue avec « duplicate column ».
        // L'erreur était propagée par `?`, ce qui faisait sortir la fonction
        // avant l'écriture : le wrapper n'écrivait jamais rien, sans bruit.
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE session (
                 id TEXT PRIMARY KEY,
                 directory TEXT,
                 time_created INTEGER,
                 context_pct INTEGER DEFAULT 0,
                 currency TEXT DEFAULT 'USD',
                 exchange_rate REAL,
                 cost_local REAL
             );
             INSERT INTO session VALUES ('a','/p',100,0,'USD',NULL,NULL);
             INSERT INTO session VALUES ('b','/p',200,0,'USD',NULL,NULL);",
        )
        .unwrap();

        let n = persist_session(&c, "/p", Some(55), Some("GBP"), Some(1.15), Some(11.5)).unwrap();
        assert_eq!(n, 1, "une seule session doit etre mise a jour");

        let lire = |id: &str| -> (i64, String, Option<f64>, Option<f64>) {
            c.query_row(
                "SELECT context_pct, currency, exchange_rate, cost_local FROM session WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
        };
        assert_eq!(lire("b"), (55, "GBP".to_string(), Some(1.15), Some(11.5)));
        assert_eq!(lire("a").0, 0, "seule la session la plus recente est touchee");
    }

    #[test]
    fn persist_session_cree_les_colonnes_manquantes() {
        // Le cas inverse : la table n'a que les colonnes d'origine, la
        // migration doit créer les trois autres et l'écriture doit aboutir.
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE session (
                 id TEXT PRIMARY KEY,
                 directory TEXT,
                 time_created INTEGER
             );
             INSERT INTO session VALUES ('a','/p',100);",
        )
        .unwrap();

        let n = persist_session(&c, "/p", Some(60), Some("EUR"), Some(1.0), Some(20.0)).unwrap();
        assert_eq!(n, 1);
        let v: (i64, String, Option<f64>, Option<f64>) = c
            .query_row(
                "SELECT context_pct, currency, exchange_rate, cost_local FROM session WHERE id = 'a'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(v, (60, "EUR".to_string(), Some(1.0), Some(20.0)));
    }

    #[test]
    fn maj_session_sans_session_correspondante_ne_echoue_pas() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE session (
                 id TEXT PRIMARY KEY, directory TEXT, time_created INTEGER,
                 context_pct INTEGER DEFAULT 0, currency TEXT DEFAULT 'USD',
                 exchange_rate REAL, cost_local REAL);",
        )
        .unwrap();
        assert_eq!(
            maj_session(&c, "/inconnu", Some(7), Some("USD"), None, None).unwrap(),
            0
        );
    }

    /// Régression P1-2 : c'est le test qui compte pour ce bug.
    ///
    /// L'ancien `UPDATE` écrivait les quatre colonnes inconditionnellement, donc
    /// une extraction vide remettait `context_pct` à 0, `currency` à « USD » et
    /// `cost_local` à NULL. Les données réelles d'une session étaient perdues à
    /// fin de la session suivante. Ici, seul le contexte est présent dans la
    /// sortie : la devise et le coût précédents doivent survivre intacts.
    #[test]
    fn une_extraction_partielle_n_ecrase_pas_les_champs_absents() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE session (
                 id TEXT PRIMARY KEY, directory TEXT, time_created INTEGER,
                 context_pct INTEGER DEFAULT 0, currency TEXT DEFAULT 'USD',
                 exchange_rate REAL, cost_local REAL);
             INSERT INTO session VALUES ('s','/p',100,31,'GBP',1.15,4.6);",
        )
        .unwrap();

        // Contexte seul : ni devise, ni taux, ni coût extraits.
        let n = maj_session(&c, "/p", Some(88), None, None, None).unwrap();
        assert_eq!(n, 1);

        let lire = |id: &str| -> (i64, String, Option<f64>, Option<f64>) {
            c.query_row(
                "SELECT context_pct, currency, exchange_rate, cost_local FROM session WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
        };
        assert_eq!(
            lire("s"),
            (88, "GBP".to_string(), Some(1.15), Some(4.6)),
            "seul context_pct doit avoir change"
        );
    }

    /// Le cas dégénéré de P1-2 : rien n'est extrait. L'ancien code émettait un
    /// UPDATE qui remettait tout à zéro/NULL. Il ne doit plus rien écrire, ni
    /// même lancer de requête.
    #[test]
    fn une_extraction_vide_ne_touche_a_rien() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE session (
                 id TEXT PRIMARY KEY, directory TEXT, time_created INTEGER,
                 context_pct INTEGER DEFAULT 0, currency TEXT DEFAULT 'USD',
                 exchange_rate REAL, cost_local REAL);
             INSERT INTO session VALUES ('s','/p',100,31,'GBP',1.15,4.6);",
        )
        .unwrap();

        let n = maj_session(&c, "/p", None, None, None, None).unwrap();
        assert_eq!(n, 0, "rien a ecrire : aucune ligne ne doit etre touchee");

        let lire = |id: &str| -> (i64, String, Option<f64>, Option<f64>) {
            c.query_row(
                "SELECT context_pct, currency, exchange_rate, cost_local FROM session WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
        };
        assert_eq!(
            lire("s"),
            (31, "GBP".to_string(), Some(1.15), Some(4.6)),
            "la session doit etre intacte"
        );
    }

    /// Le coût local dépend du taux de change : sans taux, pas de coût. Et un
    /// coût sans devise ne doit pas non plus fabriquer une devise « USD ».
    #[test]
    fn un_cout_sans_taux_ne_produit_pas_de_cost_local() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE session (
                 id TEXT PRIMARY KEY, directory TEXT, time_created INTEGER,
                 context_pct INTEGER DEFAULT 0, currency TEXT DEFAULT 'USD',
                 exchange_rate REAL, cost_local REAL);
             INSERT INTO session VALUES ('s','/p',100,31,'EUR',NULL,NULL);",
        )
        .unwrap();

        // Devise trouvée, coût trouvé, mais taux indisponible (réseau coupé).
        let n = maj_session(&c, "/p", None, Some("GBP"), None, None).unwrap();
        assert_eq!(n, 1);
        let (pct, devise, taux, cout): (i64, String, Option<f64>, Option<f64>) = c
            .query_row(
                "SELECT context_pct, currency, exchange_rate, cost_local FROM session WHERE id = 's'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!((pct, devise.as_str(), taux, cout), (31, "GBP", None, None));
    }

    /// P0-1 : la capture doit survivre à un fichier temporaire préexistant
    /// portant le même nom, sans le tronquer ni le lire par erreur.
    #[test]
    fn la_capture_ne_reutilise_pas_un_fichier_ancien() {
        let (a, _fa) = CaptureTemp::nouveau("test").unwrap();
        let (b, fb) = CaptureTemp::nouveau("test").unwrap();
        assert_ne!(
            a.path, b.path,
            "deux captures doivent avoir des fichiers distincts"
        );
        use std::io::Write;
        let mut fb = fb;
        fb.write_all(b"seconde").unwrap();
        drop(fb);
        assert_eq!(fs::read(&a.path).unwrap(), Vec::<u8>::new(), "la premiere capture doit rester vide");
        assert_eq!(fs::read(&b.path).unwrap(), b"seconde");
        fs::remove_file(&a.path).ok();
        fs::remove_file(&b.path).ok();
    }

    /// P0-1 : `restituer` rend les octets au scraping et supprime le fichier, de
    /// sorte qu'aucune capture ne s'accumule dans le répertoire temporaire.
    #[test]
    fn restituer_rend_les_octets_puis_supprime_le_fichier() {
        let (cap, mut fd) = CaptureTemp::nouveau("restitution").unwrap();
        use std::io::Write;
        fd.write_all(b"Context: 42%\nspent $12.34\n").unwrap();
        drop(fd);

        let contenu = cap.restituer(Flux::Stdout);
        assert_eq!(contenu, b"Context: 42%\nspent $12.34\n");
        assert!(!cap.path.exists(), "le fichier temporaire doit etre supprime");
    }

    #[test]
    fn extract_cost_and_currency_reversed() {
        let out = b"spent 45.67 $";
        let (curr, cost) = extract_cost_and_currency(out, b"");
        assert_eq!(curr, Some("USD".to_string()));
        assert_eq!(cost, Some(45.67));
    }
}