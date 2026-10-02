use anyhow::Result;
use dirs;
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::Client;
use rusqlite::Connection;
use rusqlite::params;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use tokio::process::Command;

static HTTP_CLIENT: Lazy<Client> = Lazy::new(|| Client::new());
static EXCHANGE_RATE_CACHE: Lazy<Mutex<HashMap<String, f64>>> = Lazy::new(|| Mutex::new(HashMap::new()));

/// Chemin du fichier de capture, unique par PID et horodatage pour que deux
/// sessions simultanées ne se marchent pas dessus.
fn chemin_capture(etiquette: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "opencode-wrap-{etiquette}-{}-{nanos}",
        std::process::id()
    ))
}

/// Échappe un argument pour une ligne de commande `/bin/sh`.
///
/// `script` ne reçoit qu'une chaîne, pas un vecteur d'arguments : la ligne de
/// commande est donc reconstruite pour `sh -c`, ce qui impose d'échapper. Sans
/// cela, un argument contenant une quote ou un `;` Allowlisting hiatuserait la
/// commande — le wrapper est lancé par le menu, mais rien n'interdit à un
/// utilisateur de lui passer des arguments.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Retire les séquences ANSI et convertit les retour chariot en sauts de ligne.
///
/// Un TUI redessine une même ligne en revenant au début (retour chariot) puis en
/// écrasant : supprimer `\r` seul collerait deux fragments distincts, et « 1 »
/// suivi de « 2% » donnerait « 12% ». En saut de ligne, chaque redessin reste
/// analysable isolément.
fn nettoyer_sortie(brut: &[u8]) -> String {
    let texte = String::from_utf8_lossy(brut);
    let mut out = String::with_capacity(texte.len());
    let mut chars = texte.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '\x1b' {
            if c == '\r' {
                out.push('\n');
            } else {
                out.push(c);
            }
            continue;
        }

        match chars.peek() {
            // Séquence CSI : sautée jusqu'à sa lettre finale.
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            // Séquence OSC : sautée jusqu'à BEL ou ST.
            Some(']') => {
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' {
                        chars.next();
                        break;
                    }
                }
            }
            // Échappement suivi d'un octet isolé.
            _ => {
                chars.next();
            }
        }
    }

    out
}

/// Conserve un extrait de la sortie lorsque rien n'a pu être extrait.
///
/// Sans cela, « le scraper ne trouve rien » est indiscernable, en base, d'une
/// session sans pourcentage de contexte : les deux laissent `context_pct` à 0.
/// Le fichier écrit permet de vérifier les motifs sur le rendu réel de opencode
/// au lieu de les supposer justes.
fn journaliser_sous_motifs(sortie: &str) {
    let path = std::env::temp_dir().join("opencode-wrap-dernier-scraping.log");
    let extrait: Vec<&str> = sortie.lines().filter(|l| !l.trim().is_empty()).collect();
    let debut = extrait.len().saturating_sub(200);
    if let Ok(mut f) = std::fs::File::create(&path) {
        let _ = f.write_all(
            format!("--- {} octets capturés ---\n", sortie.len()).as_bytes(),
        );
        for l in &extrait[debut..] {
            let _ = f.write_all(l.as_bytes());
            let _ = f.write_all(b"\n");
        }
    }
}

/// Horodatage en millisecondes, unité de `session.time_created`.
///
/// Capturé **avant** le lancement d'opencode : c'est la frontière qui
/// separates les données produites par ce run de celles des runs précédents.
fn maintenant_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Trace le cas « aucune session identifiable », dans un fichier distinct de
/// celui du scraping pour que les deux diagnostics ne s'écrasent pas.
///
/// Écrire dans une session qui n'est pas la nôtre serait pire que ne rien
/// écrire : on préfère un silence explicable à une donnée fausse.
fn journaliser_session_introuvable(project_dir: &str, debut_ms: i64) {
    let path = std::env::temp_dir().join("opencode-wrap-derniere-session.log");
    if let Ok(mut f) = std::fs::File::create(&path) {
        let _ = f.write_all(
            format!(
                "répertoire : {project_dir}\ndébut du run : {debut_ms}\n\
                 aucune session identifiable : rien n'a été écrit\n"
            )
            .as_bytes(),
        );
    }
}

/// Marge de tolérance, en millisecondes, appliquée à l'horodatage de début.
///
/// `debut_ms` est pris juste avant le `spawn`. Dans le cas normal la session
/// est créée *après*, donc la comparaison stricte suffit. La marge couvre le
/// désordre de quelques millisecondes entre l'écriture de l'horodatage et la
/// première écriture d'opencode — et le cas d'une ligne déjà présente au
/// moment du lancement. Une seconde est assez large pour cela, et assez
/// étroite pour ne jamais englober une session lancée dans le même répertoire
/// par un autre lancement antérieur.
const MARGE_MS: i64 = 1_000;

/// Identifie la session que ce run d'opencode vient d'utiliser.
///
/// Régression P1-3 : la cible était « la session la plus récente du
/// répertoire », ce qui n'est pas la même chose que « ma session ». Une
/// session reprise est souvent bien plus ancienne qu'une session créée
/// ensuite par un essai antérieur dans le même dossier : le wrapper y
/// écrivait le pourcentage de contexte de quelqu'un d'autre, silencieusement.
///
/// Résolution en quatre étapes, de la plus fiable à la plus prudente :
///
/// 1. la session ayant produit au moins un message pendant le run — c'est la
///    preuve directe que c'est elle qui a travaillé ;
/// 2. la session créée pendant le run (opencode n'a pas repris d'historique) ;
/// 3. la session touchée pendant le run (`time_updated`) : cas de la reprise ;
/// 4. aucune candidate → on ne modifie rien et on le trace.
///
/// Le critère 1 est le seul qui désambiguïse deux sessions actives du même
/// répertoire. Les suivants sont des replis : ils peuvent viser la session
/// d'un lancement concurrent, ce qui reste mieux que l'ancienne règle, et
/// le verrou par répertoire du menu (`src/lock.rs`) interdit ce cas pour les
/// lancements passés par le menu.
fn identifier_session(
    conn: &Connection,
    project_dir: &str,
    debut_ms: i64,
) -> rusqlite::Result<Option<String>> {
    let debut = debut_ms.saturating_sub(MARGE_MS);

    // Étape 1 : messages produits pendant le run.
    // `message` n'a pas d'index sur `time_created` seul (seulement
    // (session_id, time_created, id)) : c'est un balayage de la table, de
    // l'ordre de quelques dizaines de milliers de lignes, soit quelques
    // millisecondes. On ne crée pas d'index : la base appartient à opencode.
    // Une table `message` absente (autre schéma) fait simplement échouer la
    // requête, et l'on retombe sur les étapes suivantes.
    let par_messages: Option<String> = conn
        .query_row(
            "SELECT s.id FROM message m \
             JOIN session s ON s.id = m.session_id \
             WHERE m.time_created >= ?1 AND s.directory = ?2 \
             GROUP BY s.id ORDER BY max(m.time_created) DESC LIMIT 1",
            params![debut, project_dir],
            |r| r.get(0),
        )
        .ok();
    if par_messages.is_some() {
        return Ok(par_messages);
    }

    // Étape 2 : session créée pendant le run.
    let par_creation: Option<String> = conn
        .query_row(
            "SELECT id FROM session WHERE directory = ?1 AND time_created >= ?2 \
             ORDER BY time_created DESC LIMIT 1",
            params![project_dir, debut],
            |r| r.get(0),
        )
        .ok();
    if par_creation.is_some() {
        return Ok(par_creation);
    }

    // Étape 3 : session reprise, donc existante avant le run mais touchée
    // pendant. `time_updated` est le bon discriminant ici.
    let par_maj: Option<String> = conn
        .query_row(
            "SELECT id FROM session WHERE directory = ?1 AND time_updated >= ?2 \
             ORDER BY time_updated DESC LIMIT 1",
            params![project_dir, debut],
            |r| r.get(0),
        )
        .ok();
    Ok(par_maj)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let project_dir = std::env::var("OPENCODE_PROJECT_DIR").ok();

    // Horodatage de référence du run, pris avant tout lancement : c'est lui
    // qui détermine ensuite quelle session est « la nôtre ».
    let debut_ms = maintenant_ms();

    // OPENCODE_WRAP_CAPTURE=0 : opencode écrit directement dans notre terminal.
    // Le rendu est alors en direct, mais plus rien n'est scrapé, donc ni
    // contexte ni coût ne sont enregistrés. À réserver au débogage.
    let capturer = std::env::var("OPENCODE_WRAP_CAPTURE").as_deref() != Ok("0");

    let mut journal_capture: Option<PathBuf> = None;

    let mut cmd = if capturer {
        // Régression P0-2 : la capture redirigeait stdout et stderr vers des
        // fichiers réguliers, et opencode n'y écrivait alors **rien** — mesuré
        // à 0 octet, contre 7 octets pour la même commande sur un pipe. Le TUI
        // détecte un stdout qui n'est pas un terminal et se taisait : la
        // capture restait vide et aucun pourcentage de contexte n'était jamais
        // enregistré. Symptôme en base : `context_pct` à 0 partout.
        //
        // `script` fournit un vrai pseudo-terminal à opencode tout en
        // journalisant sa sortie dans un fichier. Le TUI s'affiche en direct —
        // contrepartie perdue du correctif précédent, qui restituait la sortie
        // d'un seul bloc après la fin de la session.
        let mut ligne = String::from("opencode");
        for arg in &args {
            ligne.push(' ');
            ligne.push_str(&shell_quote(arg));
        }

        let mut c = Command::new("script");
        let journal = chemin_capture("sortie");
        c.args([
            "-q",
            "-f",
            "-e",
            "-c",
            &ligne,
            journal.to_string_lossy().as_ref(),
        ]);
        journal_capture = Some(journal);
        c
    } else {
        let mut c = Command::new("opencode");
        c.args(&args);
        c
    };

    // Régression P0-1 : sans cet héritage explicite, `output()` redirigeait
    // stdin et opencode recevait un EOF immédiat — le TUI était inutilisable.
    cmd.stdin(Stdio::inherit());

    // En mode pseudo-terminal, `script` retransmet la sortie vers notre
    // terminal : il faut laisser stdout et stderr hérités, sinon le TUI
    // s'affiche dans le journal de capture au lieu de l'écran.
    if capturer {
        cmd.stdout(Stdio::inherit());
        cmd.stderr(Stdio::inherit());
    }

    // `status()` et non `output()` : on veut laisser le terminal à opencode.
    let status = match cmd.status().await {
        Ok(s) => s,
        Err(e) if capturer && e.kind() == std::io::ErrorKind::NotFound => {
            // `script` absent : session utilisable, mais sans capture.
            eprintln!(
                "opencode-wrap: « script » introuvable, capture désactivée \
                 (contexte et coût ne seront pas enregistrés)"
            );
            Command::new("opencode")
                .args(&args)
                .stdin(Stdio::inherit())
                .status()
                .await?
        }
        Err(e) => return Err(e.into()),
    };
    let exit_code = status.code().unwrap_or(1);

    // Lecture avant le traitement réseau, pour ne pas faire attendre
    // l'utilisateur pour une conversion de devise.
    let stdout_cap: Vec<u8> = match journal_capture {
        Some(chemin) => {
            let brut = std::fs::read(&chemin).unwrap_or_default();
            let _ = std::fs::remove_file(&chemin);
            // stdout et stderr sont confondus dans le journal du
            // pseudo-terminal : le scraping lit les deux.
            nettoyer_sortie(&brut).into_bytes()
        }
        None => Vec::new(),
    };
    let stderr_cap: Vec<u8> = Vec::new();

    if let Some(ref dir) = project_dir {
        if let Err(e) = update_session_data(dir, debut_ms, &stdout_cap, &stderr_cap).await {
            eprintln!("opencode-wrap: erreur mise à jour session: {e}");
        }
    }

    std::process::exit(exit_code);
}

async fn update_session_data(
    project_dir: &str,
    debut_ms: i64,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<()> {
    // Le journal du pseudo-terminal est pollué par les codes ANSI et les
    // retour chariot : sans ce nettoyage, aucun motif ne peut s'appliquer.
    let sortie = nettoyer_sortie(stdout).into_bytes();
    let sortie_err = nettoyer_sortie(stderr).into_bytes();
    let context_pct = extract_context_pct(&sortie, &sortie_err);
    let (currency, cost_original) = extract_cost_and_currency(&sortie, &sortie_err);

    // Rien d'utile trouvé : on conserve un extrait de la sortie pour pouvoir
    // vérifier les motifs sur le rendu réel au lieu de les supposer justes.
    if context_pct.is_none() && currency.is_none() {
        journaliser_sous_motifs(&String::from_utf8_lossy(&sortie));
    }

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
        debut_ms,
        context_pct,
        currency.as_deref(),
        exchange_rate,
        cost_local,
    )?;

    Ok(())
}

/// Applique les migrations de colonnes puis reporte contexte, devise et
/// coûts sur la session qu'opencode vient d'utiliser dans ce répertoire.
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
    debut_ms: i64,
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

    // Rien n'a été extrait : inutile de résoudre la session, et surtout
    // inutile de risquer une écriture.
    if context_pct.is_none() && currency.is_none() && exchange_rate.is_none() && cost_local.is_none()
    {
        return Ok(0);
    }

    // Cible : la session réellement utilisée par ce run, et pas « la plus
    // récente du dossier ». Voir `identifier_session` pour les quatre étapes
    // et pour le cas où aucune session n'est identifiable — on n'écrit alors
    // rien, plutôt que d'écrire dans celle d'un autre.
    let session_id = match identifier_session(conn, project_dir, debut_ms)? {
        Some(id) => id,
        None => {
            journaliser_session_introuvable(project_dir, debut_ms);
            return Ok(0);
        }
    };

    maj_session(
        conn,
        &session_id,
        context_pct,
        currency,
        exchange_rate,
        cost_local,
    )
}

/// Reporte contexte, devise et coûts sur la session identifiée. Retourne le
/// nombre de lignes mises à jour (0 si rien n'a été extrait), pour que
/// l'appelant puisse distinguer « rien à faire » d'une écriture effective.
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
    session_id: &str,
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

    // Cible : l'identifiant de session déjà résolu par `identifier_session`.
    // Le `WHERE id = ?` est donc un cas simple, sans sous-requête.
    let sql = format!("UPDATE session SET {set} WHERE id = ?");
    valeurs.push(Value::Text(session_id.to_string()));
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

    // Le TUI se redessine : « Context: 10 % » apparaît tôt, « Context: 80 % »
    // à la fin. Prendre la première occurrence stockait donc un pourcentage
    // périmé. On retient la dernière, celle qui reflète l'état à la sortie.
    for pattern in &patterns {
        if let Ok(re) = Regex::new(pattern) {
            if let Some(caps) = re.captures_iter(&combined).last() {
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
    fn maj_session_ecrit_reellement_sur_la_session_ciblee() {
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

        let n = maj_session(&c, "recente", Some(42), Some("EUR"), Some(0.92), Some(9.2)).unwrap();
        assert_eq!(n, 1, "une seule session doit etre mise a jour");

        let lire = |id: &str| -> (i64, String, Option<f64>, Option<f64>) {
            c.query_row(
                "SELECT context_pct, currency, exchange_rate, cost_local FROM session WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
        };

        // La session visée, et elle seule.
        assert_eq!(lire("recente"), (42, "EUR".to_string(), Some(0.92), Some(9.2)));
        assert_eq!(lire("vieille").1, "USD".to_string());
        assert_eq!(lire("autre").1, "USD".to_string(), "une autre session ne doit pas etre touchee");
    }

    #[test]
    fn maj_session_ciblee_absente_ne_echoue_pas() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE session (
                 id TEXT PRIMARY KEY, directory TEXT, time_created INTEGER,
                 context_pct INTEGER DEFAULT 0, currency TEXT DEFAULT 'USD',
                 exchange_rate REAL, cost_local REAL);",
        )
        .unwrap();
        assert_eq!(
            maj_session(&c, "inexistant", Some(7), Some("USD"), None, None).unwrap(),
            0
        );
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

        // « b » a été créée pendant le run (200 >= 150), « a » avant lui.
        let n = persist_session(&c, "/p", 150, Some(55), Some("GBP"), Some(1.15), Some(11.5))
            .unwrap();
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
        assert_eq!(lire("a").0, 0, "seule la session creee pendant le run est touchee");
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

        let n = persist_session(&c, "/p", 50, Some(60), Some("EUR"), Some(1.0), Some(20.0)).unwrap();
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
        let n = maj_session(&c, "s", Some(88), None, None, None).unwrap();
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

        let n = maj_session(&c, "s", None, None, None, None).unwrap();
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
        let n = maj_session(&c, "s", None, Some("GBP"), None, None).unwrap();
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

    /// Régression P1-3 : le cas réel, tiré de la base de l'utilisateur.
    ///
    /// La session réellement ouverte est `active`, créée il y a trois jours et
    /// reprise aujourd'hui. Une session `essai` du même répertoire a été créée
    /// *après* elle, par un test antérieur. L'ancienne règle — « la session la
    /// plus récente du répertoire » — visait `essai` et y écrivait le
    /// pourcentage de contexte de `active`, en silence.
    ///
    /// Le seul critère qui distingue les deux est l'activité pendant le run :
    /// `active` a produit des messages, `essai` n'en a produit aucun.
    #[test]
    fn la_session_reprise_est_visitee_meme_si_elle_est_plus_ancienne() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE session (
                 id TEXT PRIMARY KEY, directory TEXT,
                 time_created INTEGER, time_updated INTEGER,
                 context_pct INTEGER DEFAULT 0, currency TEXT DEFAULT 'USD',
                 exchange_rate REAL, cost_local REAL);
             CREATE TABLE message (
                 id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER);
             INSERT INTO session VALUES
                 ('active','/p',1000,5000,0,'USD',NULL,NULL),
                 ('essai','/p',2000,2000,0,'USD',NULL,NULL);
             INSERT INTO message VALUES ('m1','active',4500);",
        )
        .unwrap();

        // Run démarré à 4000 : « active » travaille, « essai » est_muette.
        let identifiee = identifier_session(&c, "/p", 4000).unwrap();
        assert_eq!(
            identifiee.as_deref(),
            Some("active"),
            "la session qui a produit des messages pendant le run est la nôtre"
        );

        let n = persist_session(&c, "/p", 4000, Some(73), None, None, None).unwrap();
        assert_eq!(n, 1);

        let lire = |id: &str| -> i64 {
            c.query_row("SELECT context_pct FROM session WHERE id = ?1", [id], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(lire("active"), 73, "notre session reçoit le contexte");
        assert_eq!(lire("essai"), 0, "la session d'un essai antérieur reste intacte");
    }

    /// Reprise sans aucun message produit (opencode refermé aussitôt) : la
    /// session a été créée avant le run mais touchée pendant. `time_updated`
    /// est alors le seul discriminant.
    #[test]
    fn une_reprise_sans_message_est_identifiee_par_son_horodatage_de_maj() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE session (
                 id TEXT PRIMARY KEY, directory TEXT,
                 time_created INTEGER, time_updated INTEGER,
                 context_pct INTEGER DEFAULT 0, currency TEXT DEFAULT 'USD',
                 exchange_rate REAL, cost_local REAL);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER);
             INSERT INTO session VALUES
                 ('active','/p',1000,4000,0,'USD',NULL,NULL),
                 ('essai','/p',1500,1500,0,'USD',NULL,NULL);",
        )
        .unwrap();

        // Run démarré à 3000 : la marge d'une seconde ramène le seuil à 2000,
        // donc « essai » (créée et figée à 1500) est hors du run.
        let identifiee = identifier_session(&c, "/p", 3000).unwrap();
        assert_eq!(identifiee.as_deref(), Some("active"));
    }

    /// Le filet de sécurité : aucune session ne correspond au run. Écrire dans
    /// « la plus récente du dossier » revenait à choisir au hasard ; on ne
    /// touche à rien et on trace le fait.
    #[test]
    fn aucune_session_identifiable_ne_touche_a_rien() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE session (
                 id TEXT PRIMARY KEY, directory TEXT,
                 time_created INTEGER, time_updated INTEGER,
                 context_pct INTEGER DEFAULT 0, currency TEXT DEFAULT 'USD',
                 exchange_rate REAL, cost_local REAL);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER);
             INSERT INTO session VALUES ('loin','/p',1000,1000,31,'GBP',1.15,4.6);",
        )
        .unwrap();

        assert_eq!(identifier_session(&c, "/p", 9000).unwrap(), None);

        let n = persist_session(&c, "/p", 9000, Some(99), Some("EUR"), None, None).unwrap();
        assert_eq!(n, 0, "rien ne doit être écrit quand aucune session n'est identifiable");

        let v: (i64, String, Option<f64>, Option<f64>) = c
            .query_row(
                "SELECT context_pct, currency, exchange_rate, cost_local FROM session WHERE id = 'loin'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(v, (31, "GBP".to_string(), Some(1.15), Some(4.6)), "la session doit rester intacte");
    }

    /// Un schéma sans table `message` (autre version d'opencode) ne doit pas
    /// empêcher l'écriture : les étapes 2 et 3 prennent le relais.
    #[test]
    fn l_absence_de_table_message_ne_bloque_pas_l_ecriture() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE session (
                 id TEXT PRIMARY KEY, directory TEXT,
                 time_created INTEGER, time_updated INTEGER,
                 context_pct INTEGER DEFAULT 0, currency TEXT DEFAULT 'USD',
                 exchange_rate REAL, cost_local REAL);
             INSERT INTO session VALUES ('neuve','/p',5000,5000,0,'USD',NULL,NULL);",
        )
        .unwrap();

        let n = persist_session(&c, "/p", 4000, Some(12), None, None, None).unwrap();
        assert_eq!(n, 1);
        let pct: i64 = c
            .query_row("SELECT context_pct FROM session WHERE id = 'neuve'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pct, 12);
    }

    /// Le TUI se redessine à chaque événement : la même ligne apparaît en
    /// début de session (« Context: 3 % ») puis bien plus tard (« Context:
    /// 81 % »). Le pourcentage à retenir est le dernier, pas le premier :
    /// l'inverse stockait un état périmé et trahissait « le contexte baisse ».
    #[test]
    fn le_contexte_retient_la_derniere_occurrence_du_redessin() {
        let sortie = b"Context: 3% used\ntravail en cours\nContext: 81% used\n";
        assert_eq!(extract_context_pct(sortie, b""), Some(81));
    }

    /// Même principe sur l'ordre inverse des motifs : la dernière ligne lisible
    /// reste prioritaire quand le motif générique « N % used » s'applique.
    #[test]
    fn le_contexte_suit_le_dernier_pourcentage_affiche() {
        let sortie = b"  12% used   \r\n  47% used   \r\n  93% used   \r\n";
        assert_eq!(extract_context_pct(sortie, b""), Some(93));
    }

    /// P0-1, adapté au pseudo-terminal : le journal de capture ne doit pas
    /// être réutilisé d'une session à l'autre, sinon le scraping lirait la
    /// sortie de la session précédente.
    #[test]
    fn la_capture_ne_reutilise_pas_un_fichier_ancien() {
        let a = chemin_capture("test");
        let b = chemin_capture("test");
        assert_ne!(a, b, "deux captures doivent avoir des fichiers distincts");
    }

    /// P0-2 : le journal du pseudo-terminal est pollué par les séquences ANSI
    /// et les retour chariot. Sans nettoyage, « 42% » était illisible et le
    /// contexte n'était jamais extrait.
    #[test]
    fn nettoyer_sortie_expose_le_contexte_d_un_tui() {
        let brut = b"\x1b[2K\r  Context: 42% used\r\n\x1b[0m\x1b[38;5;33mfin\x1b[0m";
        let sortie = nettoyer_sortie(brut);
        assert!(!sortie.contains('\u{1b}'), "ANSI résiduel : {sortie:?}");
        assert_eq!(extract_context_pct(sortie.as_bytes(), b""), Some(42));
    }

    /// Deux redessins de la même ligne ne doivent pas se coller : sans le saut
    /// de ligne au retour chariot, « 1 » puis « 2% » donneraient « 12% ».
    #[test]
    fn nettoyer_sortie_ne_colle_pas_les_redessins() {
        let sortie = nettoyer_sortie(b"Context: 1\r2%");
        assert_eq!(sortie, "Context: 1\n2%");
    }

    #[test]
    fn shell_quote_echappe_les_quotes() {
        assert_eq!(shell_quote("simple"), "'simple'");
        assert_eq!(shell_quote("avec espace"), "'avec espace'");
        assert_eq!(shell_quote("l'arc"), r"'l'\''arc'");
    }

    /// `script` ne reçoit qu'une chaîneInterpretation de sh -c : un argument
    /// contenant une quote ou un séparateur de commande doit revenir intact,
    /// sans jamais être exécuté. Test de comportement, pas de forme.
    #[test]
    fn shell_quote_ne_permet_pas_d_injection() {
        let piege = "x'; touch /tmp/opencode-wrap-pwned; echo '";
        let temoin = std::env::temp_dir().join("opencode-wrap-pwned");
        let _ = std::fs::remove_file(&temoin);

        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("printf '%s' {}", shell_quote(piege)))
            .output()
            .expect("sh disponible");

        assert_eq!(String::from_utf8_lossy(&out.stdout), piege);
        assert!(!temoin.exists(), "l'argument a été interprété par le shell");
    }

    #[test]
    fn extract_cost_and_currency_reversed() {
        let out = b"spent 45.67 $";
        let (curr, cost) = extract_cost_and_currency(out, b"");
        assert_eq!(curr, Some("USD".to_string()));
        assert_eq!(cost, Some(45.67));
    }
}