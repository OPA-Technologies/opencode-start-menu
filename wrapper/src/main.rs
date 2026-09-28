use anyhow::Result;
use dirs;
use rusqlite::Connection;
use std::io::Write;
use std::path::PathBuf;
use tokio::process::Command;

#[tokio::main]
async fn main() -> Result<()> {
    // Récupérer les arguments passés à opencode
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Récupérer le répertoire du projet depuis la variable d'environnement
    let project_dir = std::env::var("OPENCODE_PROJECT_DIR").ok();

    // Lancer opencode avec les arguments, capturer la sortie
    let mut cmd = Command::new("opencode");
    cmd.args(&args);

    let output = cmd.output().await?;
    let exit_code = output.status.code().unwrap_or(1);

    // Afficher la sortie d'opencode sur stdout/stderr
    if !output.stdout.is_empty() {
        std::io::stdout().write_all(&output.stdout)?;
    }
    if !output.stderr.is_empty() {
        std::io::stderr().write_all(&output.stderr)?;
    }

    // Après la fin d'opencode, essayer d'extraire le % contexte
    // et l'écrire dans la base SQLite
    if let Some(ref dir) = project_dir {
        if let Err(e) = update_context_pct(dir, &output).await {
            eprintln!("opencode-wrap: erreur mise à jour context_pct: {e}");
        }
    }

    std::process::exit(exit_code);
}

async fn update_context_pct(project_dir: &str, output: &std::process::Output) -> Result<()> {
    // Extraire le % contexte depuis la sortie d'opencode
    let context_pct = extract_context_pct(&output.stdout, &output.stderr);

    // Chemin vers la base opencode
    let db_path = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("opencode/opencode.db");

    if !db_path.exists() {
        return Ok(());
    }

    // Mettre à jour la dernière session du projet
    let conn = Connection::open(&db_path)?;
    conn.execute(
        "UPDATE session SET context_pct = ? WHERE directory = ? ORDER BY time_created DESC LIMIT 1",
        rusqlite::params![context_pct as i64, project_dir],
    )?;

    Ok(())
}

fn extract_context_pct(stdout: &[u8], stderr: &[u8]) -> u8 {
    // Convertir les sorties en texte
    let stdout_str = String::from_utf8_lossy(stdout);
    let stderr_str = String::from_utf8_lossy(stderr);
    let combined = format!("{}\n{}", stdout_str, stderr_str);

    // Patterns courants pour le % contexte dans la sortie d'opencode
    // On cherche des patterns comme "Context: XX%", "context: XX%", "XX% context", etc.
    let patterns = [
        r"(?i)context\s*[:=]\s*(\d{1,3})\s*%",
        r"(?i)(\d{1,3})\s*%\s*context",
        r"(?i)context\s*(\d{1,3})\s*%",
        r"(\d{1,3})\s*%\s*used",
    ];

    for pattern in &patterns {
        if let Ok(re) = regex::Regex::new(pattern) {
            if let Some(caps) = re.captures(&combined) {
                if let Some(m) = caps.get(1) {
                    if let Ok(pct) = m.as_str().parse::<u8>() {
                        return pct.min(100);
                    }
                }
            }
        }
    }

    // Si pas trouvé, essayer de parser depuis la sortie brute pour des patterns ANSI
    // ou des lignes de statut final
    for line in combined.lines().rev().take(50) {
        let line_lower = line.to_lowercase();
        if line_lower.contains("context") && line_lower.contains('%') {
            // Extraire les chiffres près de %
            let re = regex::Regex::new(r"(\d{1,3})\s*%").unwrap();
            if let Some(caps) = re.captures(line) {
                if let Ok(pct) = caps.get(1).unwrap().as_str().parse::<u8>() {
                    return pct.min(100);
                }
            }
        }
    }

    // Par défaut : 0 (inconnu)
    0
}