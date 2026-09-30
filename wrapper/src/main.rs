use anyhow::Result;
use dirs;
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::Client;
use rusqlite::Connection;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use tokio::process::Command;

static HTTP_CLIENT: Lazy<Client> = Lazy::new(|| Client::new());
static EXCHANGE_RATE_CACHE: Lazy<Mutex<HashMap<String, f64>>> = Lazy::new(|| Mutex::new(HashMap::new()));

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let project_dir = std::env::var("OPENCODE_PROJECT_DIR").ok();

    let mut cmd = Command::new("opencode");
    cmd.args(&args);

    let output = cmd.output().await?;
    let exit_code = output.status.code().unwrap_or(1);

    if !output.stdout.is_empty() {
        std::io::stdout().write_all(&output.stdout)?;
    }
    if !output.stderr.is_empty() {
        std::io::stderr().write_all(&output.stderr)?;
    }

    if let Some(ref dir) = project_dir {
        if let Err(e) = update_session_data(dir, &output).await {
            eprintln!("opencode-wrap: erreur mise à jour session: {e}");
        }
    }

    std::process::exit(exit_code);
}

async fn update_session_data(project_dir: &str, output: &std::process::Output) -> Result<()> {
    let context_pct = extract_context_pct(&output.stdout, &output.stderr);
    let (currency, cost_original) = extract_cost_and_currency(&output.stdout, &output.stderr);

    let db_path = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("opencode/opencode.db");

    if !db_path.exists() {
        return Ok(());
    }

    let conn = Connection::open(&db_path)?;

    let exchange_rate = if let Some(ref curr) = currency {
        fetch_exchange_rate(curr, "EUR").await
    } else {
        None
    };

    let cost_local = if let (Some(rate), Some(cost)) = (exchange_rate, cost_original) {
        Some(cost * rate)
    } else {
        None
    };

    let currency_str = currency.unwrap_or_else(|| "USD".to_string());

    persist_session(
        &conn,
        project_dir,
        context_pct as i64,
        &currency_str,
        exchange_rate,
        cost_local,
    )?;

    Ok(())
}

/// Applique les migrations de colonnes puis reporte contexte, devise et
/// coûts sur la session la plus récente du répertoire.
///
/// Les `ALTER TABLE ADD COLUMN` sont volontairement ignorés : les colonnes
/// existent déjà après la première exécution (et le menu les crée de son
/// côté), donc SQLite renvoie « duplicate column ». Propager cette erreur
/// faisait sortir la fonction avant l'écriture — le wrapper n'écrivait donc
/// jamais rien, silencieusement.
fn persist_session(
    conn: &Connection,
    project_dir: &str,
    context_pct: i64,
    currency: &str,
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
/// session ne correspond), pour que l'appelant puisse distinguer « rien à
/// faire » d'une écriture effective.
fn maj_session(
    conn: &Connection,
    project_dir: &str,
    context_pct: i64,
    currency: &str,
    exchange_rate: Option<f64>,
    cost_local: Option<f64>,
) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE session SET context_pct = ?, currency = ?, exchange_rate = ?, cost_local = ? \
         WHERE id = (SELECT id FROM session WHERE directory = ? ORDER BY time_created DESC LIMIT 1)",
        rusqlite::params![context_pct, currency, exchange_rate, cost_local, project_dir],
    )
}

fn extract_context_pct(stdout: &[u8], stderr: &[u8]) -> u8 {
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
                        return pct.min(100);
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
                    return pct.min(100);
                }
            }
        }
    }

    0
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

    #[test]
    fn extract_context_pct_basic() {
        assert_eq!(extract_context_pct(b"Context: 50%", b""), 50);
        assert_eq!(extract_context_pct(b"context: 75%", b""), 75);
        assert_eq!(extract_context_pct(b"50% context", b""), 50);
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

        let n = maj_session(&c, "/p", 42, "EUR", Some(0.92), Some(9.2)).unwrap();
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

        let n = persist_session(&c, "/p", 55, "GBP", Some(1.15), Some(11.5)).unwrap();
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

        let n = persist_session(&c, "/p", 60, "EUR", Some(1.0), Some(20.0)).unwrap();
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
        assert_eq!(maj_session(&c, "/inconnu", 7, "USD", None, None).unwrap(), 0);
    }

    #[test]
    fn extract_cost_and_currency_reversed() {
        let out = b"spent 45.67 $";
        let (curr, cost) = extract_cost_and_currency(out, b"");
        assert_eq!(curr, Some("USD".to_string()));
        assert_eq!(cost, Some(45.67));
    }
}