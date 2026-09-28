use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use crate::perms;

// Nouvelles structures pour les données d'usage
#[derive(Debug, Clone, Default)]
pub(crate) struct LastSession {
    pub tokens: u64,
    pub cost: f64,
    pub model: String,
    pub agent: String,
    pub time_created: i64,
    pub context_pct: u8,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CumulativeUsage {
    pub total_tokens: u64,
    pub total_cost: f64,
    pub session_count: usize,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ProjectUsage {
    pub cumulative: CumulativeUsage,
    pub last_session: Option<LastSession>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct Project {
    name: String,
    path: String,
    description: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    token: String,
    #[serde(default)]
    used_pct: String,
    #[serde(default)]
    spent: String,
    #[serde(default)]
    last_model: String,
    #[serde(default)]
    total_tokens: String,
    #[serde(default)]
    total_cost: String,
    #[serde(default)]
    session_count: String,
    #[serde(default)]
    last_session_tokens: String,
    #[serde(default)]
    last_session_cost: String,
    #[serde(default)]
    last_agent: String,
    #[serde(default)]
    last_time_created: String,
    #[serde(default)]
    status_debug: String,
    #[serde(default)]
    version_debug: String,
    #[serde(default)]
    status_release: String,
    #[serde(default)]
    version_release: String,
    #[serde(default)]
    last_compile_debug_secs: u64,
    #[serde(default)]
    last_compile_release_secs: u64,
    // Champ historique (uniquement pour migrer les anciens registres).
    #[serde(default)]
    last_compile_secs: u64,
    // Marque si la version a été éditée manuellement (pour éviter AutoPatch bump).
    #[serde(default)]
    version_manually_edited: bool,
    #[serde(default = "default_true")]
    active: bool,
}

fn default_true() -> bool {
    true
}

pub(crate) const STATUSES: &[&str] = &[
    "Study",
    "Alpha",
    "Beta",
    "Debug",
    "Beta-Release",
    "Release",
    "Stable",
    "Deprecated",
];

pub(crate) fn validate_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

pub(crate) fn validate_status(s: &str) -> bool {
    STATUSES.iter().any(|known| *known == s)
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct Registry {
    #[serde(default)]
    projects: HashMap<String, Project>,
}

#[derive(Clone)]
pub(crate) struct ProjectEntry {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) description: String,
    pub(crate) status: String,
    pub(crate) version: String,
    pub(crate) token: String,
    pub(crate) used_pct: String,
    pub(crate) spent: String,
    pub(crate) last_model: String,
    pub(crate) modified: String,
    // Nouveaux champs pour les données d'usage
    pub(crate) total_tokens: String,
    pub(crate) total_cost: String,
    pub(crate) session_count: String,
    pub(crate) last_session_tokens: String,
    pub(crate) last_session_cost: String,
    pub(crate) last_agent: String,
    pub(crate) last_time_created: String,
    pub(crate) last_session_context_pct: String,
    pub(crate) status_debug: String,
    pub(crate) version_debug: String,
    pub(crate) status_release: String,
    pub(crate) version_release: String,
    pub(crate) debug_modified: String,
    pub(crate) release_modified: String,
    /// Références (epoch secondes) de la dernière compilation détectée par
    /// variante (mtimes de target/debug et target/release). 0 = jamais.
    pub(crate) last_compile_debug_secs: u64,
    pub(crate) last_compile_release_secs: u64,
    /// Marque si la version a été éditée manuellement dans le menu
    /// (pour éviter le bump AutoPatch au prochain rescan).
    pub(crate) version_manually_edited: bool,
}

pub(crate) fn load_registry(path: &Path) -> Result<Vec<ProjectEntry>> {
    ensure_regular(path)?;
    if !path.exists() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        write_private(path, r#"{"projects":{}}"#)?;
    }
    let content = fs::read_to_string(path)?;
    let registry: Registry = serde_json::from_str(&content)?;

    let projects = registry
        .projects
        .into_iter()
        .map(|(id, p)| {
            let dir = PathBuf::from(shellexpand::tilde(&p.path).to_string());
            let modified = last_modified(&dir).unwrap_or_else(|| "—".to_string());
            let debug_modified =
                variant_modified(&dir, "debug").unwrap_or_else(|| "jamais compilée".to_string());
            let release_modified =
                variant_modified(&dir, "release").unwrap_or_else(|| "jamais compilée".to_string());
            // Migration des anciens champs uniques status/version vers les variantes
            // Debug/Release si les nouvelles valeurs sont absentes.
            let status_debug =
                if p.status_debug.is_empty() { p.status.clone() } else { p.status_debug.clone() };
            let version_debug =
                if p.version_debug.is_empty() { p.version.clone() } else { p.version_debug.clone() };
            let status_release =
                if p.status_release.is_empty() { status_debug.clone() } else { p.status_release.clone() };
            let version_release =
                if p.version_release.is_empty() { version_debug.clone() } else { p.version_release.clone() };
            // Migration de la référence unique historique : si les deux
            // références par variante sont absentes (0), le registre étant
            // alors pré-dissociation, l'ancienne référence est attribuée aux
            // deux variantes (aucune n'est considérée « en retard » au départ).
            let (last_debug_secs, last_release_secs) =
                if p.last_compile_debug_secs == 0 && p.last_compile_release_secs == 0 {
                    (p.last_compile_secs, p.last_compile_secs)
                } else {
                    (p.last_compile_debug_secs, p.last_compile_release_secs)
                };
            ProjectEntry {
                id,
                name: p.name,
                path: dir,
                description: p.description,
                status: p.status,
                version: p.version,
                token: p.token,
                used_pct: p.used_pct,
                spent: p.spent,
                last_model: p.last_model,
                modified,
                total_tokens: p.total_tokens,
                total_cost: p.total_cost,
                session_count: p.session_count,
                last_session_tokens: p.last_session_tokens,
                last_session_cost: p.last_session_cost,
                last_agent: p.last_agent,
                last_time_created: p.last_time_created,
                last_session_context_pct: String::new(),
                status_debug,
                version_debug,
                status_release,
                version_release,
                debug_modified,
                release_modified,
                last_compile_debug_secs: last_debug_secs,
                last_compile_release_secs: last_release_secs,
                version_manually_edited: p.version_manually_edited,
            }
        })
        .collect::<Vec<ProjectEntry>>();

    Ok(projects)
}

// Nouvelle fonction pour charger les données d'usage (cumulées + dernière session)
pub(crate) fn load_usage(db_path: &Path) -> HashMap<PathBuf, ProjectUsage> {
    let mut usage = HashMap::new();
    let Ok(conn) = Connection::open(db_path) else {
        return usage;
    };
    // Migration : ajouter la colonne context_pct si elle n'existe pas.
    let _ = conn.execute(
        "ALTER TABLE session ADD COLUMN context_pct INTEGER DEFAULT 0",
        [],
    );
    let mut stmt = match conn.prepare(
        "SELECT directory, COALESCE(cost,0), \
         COALESCE(tokens_input,0) + COALESCE(tokens_output,0) + COALESCE(tokens_reasoning,0), \
         model, agent, time_created, COALESCE(context_pct,0) \
         FROM session WHERE directory IS NOT NULL ORDER BY time_created DESC",
    ) {
        Ok(s) => s,
        Err(_) => return usage,
    };
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, f64>(1)?,
            row.get::<_, i64>(2)? as u64,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, u8>(6)?,
        ))
    });
    match rows {
        Ok(iter) => {
            for r in iter.flatten() {
                let (dir, cost, tokens, model, agent, t, context_pct) = r;
                let entry = usage
                    .entry(PathBuf::from(dir))
                    .or_insert_with(|| ProjectUsage {
                        cumulative: CumulativeUsage::default(),
                        last_session: None,
                    });
                // Cumuler les totaux
                entry.cumulative.total_tokens += tokens;
                entry.cumulative.total_cost += cost;
                entry.cumulative.session_count += 1;
                // Dernière session (la première rencontrée car ORDER BY time_created DESC)
                if entry.last_session.is_none() {
                    let model_id = model.and_then(|m| {
                        serde_json::from_str::<serde_json::Value>(&m)
                            .ok()
                            .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()))
                    });
                    let agent_cap = agent.map(|a| capitalize(&a));
                    entry.last_session = Some(LastSession {
                        tokens,
                        cost,
                        model: model_id.unwrap_or_default(),
                        agent: agent_cap.unwrap_or_default(),
                        time_created: t,
                        context_pct,
                    });
                }
            }
        }
        Err(_) => {}
    }
    usage
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

pub(crate) fn save_registry(path: &Path, projects: &[ProjectEntry]) -> Result<()> {
    ensure_regular(path)?;
    let mut map = HashMap::new();
    for p in projects {
        map.insert(
            p.id.clone(),
            Project {
                name: p.name.clone(),
                path: p.path.to_string_lossy().to_string(),
                description: p.description.clone(),
                status: p.status.clone(),
                version: p.version.clone(),
                token: p.token.clone(),
                used_pct: p.used_pct.clone(),
                spent: p.spent.clone(),
                last_model: p.last_model.clone(),
                total_tokens: p.total_tokens.clone(),
                total_cost: p.total_cost.clone(),
                session_count: p.session_count.clone(),
                last_session_tokens: p.last_session_tokens.clone(),
                last_session_cost: p.last_session_cost.clone(),
                last_agent: p.last_agent.clone(),
                last_time_created: p.last_time_created.clone(),
                status_debug: p.status_debug.clone(),
                version_debug: p.version_debug.clone(),
                status_release: p.status_release.clone(),
                version_release: p.version_release.clone(),
                last_compile_debug_secs: p.last_compile_debug_secs,
                last_compile_release_secs: p.last_compile_release_secs,
                // Ancien champ : remis à zéro après migration (géré par default).
                last_compile_secs: 0,
                version_manually_edited: p.version_manually_edited,
                active: true,
            },
        );
    }
    let registry = Registry { projects: map };
    let tmp = path.with_extension("tmp");
    write_private(&tmp, &serde_json::to_string_pretty(&registry)?)?;
    fs::rename(tmp, path)?;
    Ok(())
}

pub(crate) fn is_excluded(path: &Path) -> bool {
    path.file_name()
        .map(|n| {
            let n = n.to_string_lossy();
            contains_casefold(&n, EXCLUDED_DIRS)
        })
        .unwrap_or(false)
}

fn contains_casefold(name: &str, list: &[&str]) -> bool {
    let lower = name.to_lowercase();
    list.iter().any(|e| e.to_lowercase() == lower)
}

const EXCLUDED_DIRS: &[&str] = &["jdk-21"];

pub(crate) fn reindex_from_disk(root: &Path, projects: &mut Vec<ProjectEntry>) -> Result<usize> {
    if !root.is_dir() {
        return Ok(0);
    }
    let mut added = 0usize;
    let known: HashSet<PathBuf> = projects.iter().map(|p| p.path.clone()).collect();
    let mut dirs = Vec::new();
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    for dir in dirs {
        if known.contains(&dir) || is_excluded(&dir) {
            continue;
        }
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let modified = last_modified(&dir).unwrap_or_else(|| "—".to_string());
        let debug_modified =
            variant_modified(&dir, "debug").unwrap_or_else(|| "jamais compilée".to_string());
        let release_modified =
            variant_modified(&dir, "release").unwrap_or_else(|| "jamais compilée".to_string());
        projects.push(ProjectEntry {
            id: name.clone(),
            name,
            path: dir.clone(),
            description: String::new(),
            status: String::new(),
            version: String::new(),
            token: String::new(),
            used_pct: String::new(),
            spent: String::new(),
            last_model: String::new(),
            modified,
            total_tokens: String::new(),
            total_cost: String::new(),
            session_count: String::new(),
            last_session_tokens: String::new(),
            last_session_cost: String::new(),
            last_agent: String::new(),
            last_time_created: String::new(),
            last_session_context_pct: String::new(),
            status_debug: String::new(),
            version_debug: String::new(),
            status_release: String::new(),
            version_release: String::new(),
            debug_modified,
            release_modified,
            last_compile_debug_secs: 0,
            last_compile_release_secs: 0,
            version_manually_edited: false,
        });
        added += 1;
    }
    Ok(added)
}

fn write_private(path: &Path, content: &str) -> Result<()> {
    fs::write(path, content)?;
    perms::private_file(path)?;
    Ok(())
}

fn ensure_regular(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path)
      {
        Ok(meta) if meta.file_type().is_symlink() => {
          Err(anyhow!("refus : {} est un lien symbolique", path.display()))
        }
        _ => Ok(()),
      }
}

pub(crate) fn last_modified_time(dir: &Path) -> Option<std::time::SystemTime> {
    if !dir.exists() {
        return None;
    }
    let mut best: Option<std::time::SystemTime> = None;
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        if let Ok(meta) = entry.metadata()
            && let Ok(mtime) = meta.modified()
            && best.is_none_or(|b| mtime > b)
        {
            best = Some(mtime);
        }
    }
    best.or_else(|| fs::metadata(dir).ok().and_then(|m| m.modified().ok()))
}

fn format_mod_time(t: std::time::SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Local> = t.into();
    dt.format("%d/%m/%Y %H:%M").to_string()
}

fn last_modified(dir: &Path) -> Option<String> {
    last_modified_time(dir).map(format_mod_time)
}

pub(crate) fn variant_modified(dir: &Path, profile: &str) -> Option<String> {
    last_modified_time(&dir.join("target").join(profile)).map(format_mod_time)
}

/// mtime (SystemTime) du fichier le plus récent sous `target/{profile}`.
pub(crate) fn variant_modified_time(dir: &Path, profile: &str) -> Option<std::time::SystemTime>
  {
    last_modified_time(&dir.join("target").join(profile))
  }

#[cfg(test)]
mod tests
  {
    use std::{fs, path::PathBuf};

    use super::{load_registry, save_registry, validate_status, validate_version, ProjectEntry, STATUSES};

    fn temp_registry(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("op-start-menu-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn assert_0600(path: &std::path::Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        #[cfg(not(unix))]
        {
            let _ = path;
        }
    }

    #[test]
    fn load_creates_empty_registry_0600() {
        let dir = temp_registry("empty");
        let path = dir.join("opencode/projects.json");

        let projects = load_registry(&path).unwrap();

        assert!(projects.is_empty());
        assert_0600(&path);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_then_load_round_trip() {
        let dir = temp_registry("roundtrip");
        let path = dir.join("opencode/projects.json");
        load_registry(&path).unwrap();

        save_registry(
            &path,
            &[ProjectEntry {
                id: "test".to_string(),
                name: "Projet Test".to_string(),
                path: PathBuf::from("/tmp/nonexistent-proj"),
                description: "desc".to_string(),
                status: "En-cours Beta".to_string(),
                version: "1.0.0".to_string(),
                token: String::new(),
                used_pct: String::new(),
                spent: String::new(),
                last_model: String::new(),
                modified: "—".to_string(),
                total_tokens: String::new(),
                total_cost: String::new(),
                session_count: String::new(),
                last_session_tokens: String::new(),
                last_session_cost: String::new(),
                last_agent: String::new(),
                last_time_created: String::new(),
                last_session_context_pct: String::new(),
                status_debug: String::new(),
                version_debug: String::new(),
                status_release: String::new(),
                version_release: String::new(),
                debug_modified: String::new(),
                release_modified: String::new(),
                last_compile_debug_secs: 0,
                last_compile_release_secs: 0,
                version_manually_edited: false,
            }],
        )
        .unwrap();

        assert_0600(&path);

        let loaded = load_registry(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "test");
        assert_eq!(loaded[0].name, "Projet Test");
        assert_eq!(loaded[0].path, PathBuf::from("/tmp/nonexistent-proj"));
        assert_eq!(loaded[0].status, "En-cours Beta");
        assert_eq!(loaded[0].version, "1.0.0");
        assert_eq!(loaded[0].modified, "—");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_preserves_0600_after_multiple_writes() {
        let dir = temp_registry("multi");
        let path = dir.join("opencode/projects.json");
        load_registry(&path).unwrap();

        for i in 0..3 {
            save_registry(
                &path,
                &[ProjectEntry {
                    id: format!("p{i}"),
                    name: format!("Projet {i}"),
                    path: PathBuf::from("/tmp/nonexistent-proj"),
                    description: String::new(),
                    status: String::new(),
                    version: String::new(),
                    token: String::new(),
                    used_pct: String::new(),
                    spent: String::new(),
                    last_model: String::new(),
                    modified: "—".to_string(),
                    total_tokens: String::new(),
                    total_cost: String::new(),
                    session_count: String::new(),
                    last_session_tokens: String::new(),
                    last_session_cost: String::new(),
                    last_agent: String::new(),
                    last_time_created: String::new(),
                    last_session_context_pct: String::new(),
                    status_debug: String::new(),
                    version_debug: String::new(),
                    status_release: String::new(),
                    version_release: String::new(),
                    debug_modified: String::new(),
                    release_modified: String::new(),
                    last_compile_debug_secs: 0,
                    last_compile_release_secs: 0,
                    version_manually_edited: false,
                }],
            )
            .unwrap();
            assert_0600(&path);
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_registry() {
        let dir = temp_registry("symlink");
        let target = dir.join("target.json");
        let link = dir.join("link.json");
        fs::write(&target, "{}").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(load_registry(&link).is_err());
        assert!(save_registry(&link, &[]).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn validate_version_accepts_x_y_z() {
        assert!(validate_version("1.2.3"));
        assert!(validate_version("0.0.1"));
        assert!(validate_version("123.0.45"));
        assert!(!validate_version("1.2"));
        assert!(!validate_version("1.2.3.4"));
        assert!(!validate_version("1..3"));
        assert!(!validate_version("1.2.beta"));
        assert!(!validate_version(""));
        assert!(!validate_version("1.2-3"));
    }

    #[test]
    fn validate_status_against_fixed_set() {
        assert!(STATUSES.contains(&"Beta"));
        for known in STATUSES {
            assert!(validate_status(known));
        }
        assert!(!validate_status("En cours"));
        assert!(!validate_status("Preview"));
        assert!(!validate_status(""));
    }

    #[test]
    fn load_migrates_legacy_status_version_to_both_variants() {
        let dir = temp_registry("migrate");
        let path = dir.join("opencode/projects.json");
        load_registry(&path).unwrap();

        let legacy = r#"{"projects":{"old":{"name":"Legacy","path":"/tmp/nonexistent-proj","description":"d","status":"En-cours Beta","version":"2.1.0"}}}"#;
        fs::write(&path, legacy).unwrap();

        let loaded = load_registry(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].status_debug, "En-cours Beta");
        assert_eq!(loaded[0].version_debug, "2.1.0");
        assert_eq!(loaded[0].status_release, "En-cours Beta");
        assert_eq!(loaded[0].version_release, "2.1.0");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_migrates_legacy_last_compile_secs_to_both_refs() {
        let dir = temp_registry("migrate_compile");
        let path = dir.join("opencode/projects.json");
        load_registry(&path).unwrap();

        // Registre ancien avec last_compile_secs non-nul, sans les nouveaux champs
        let legacy = r#"{"projects":{"old":{"name":"Legacy","path":"/tmp/nonexistent-proj","description":"d","status":"Debug","version":"1.0.0","last_compile_secs":1234567890}}}"#;
        fs::write(&path, legacy).unwrap();

        let loaded = load_registry(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        // L'ancienne référence doit être dupliquée vers les deux variantes
        assert_eq!(loaded[0].last_compile_debug_secs, 1234567890);
        assert_eq!(loaded[0].last_compile_release_secs, 1234567890);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_migrates_version_manually_edited_default_false() {
        let dir = temp_registry("migrate_edited");
        let path = dir.join("opencode/projects.json");
        load_registry(&path).unwrap();

        // Registre ancien sans le champ version_manually_edited
        let legacy = r#"{"projects":{"old":{"name":"Legacy","path":"/tmp/nonexistent-proj","description":"d","status":"Debug","version":"1.0.0"}}}"#;
        fs::write(&path, legacy).unwrap();

        let loaded = load_registry(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        // Le champ doit être par défaut à false
        assert!(!loaded[0].version_manually_edited);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_keeps_existing_debug_release_variants() {
        let dir = temp_registry("keepvar");
        let path = dir.join("opencode/projects.json");
        load_registry(&path).unwrap();

        let json = r#"{"projects":{"old":{"name":"Legacy","path":"/tmp/nonexistent-proj","description":"d","status":"legacy","version":"9.9.9","status_debug":"Debug","version_debug":"1.1.1","status_release":"Release","version_release":"2.2.2"}}}"#;
        fs::write(&path, json).unwrap();

        let loaded = load_registry(&path).unwrap();
        assert_eq!(loaded[0].status_debug, "Debug");
        assert_eq!(loaded[0].version_debug, "1.1.1");
        assert_eq!(loaded[0].status_release, "Release");
        assert_eq!(loaded[0].version_release, "2.2.2");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn variant_modified_returns_none_without_target_dir() {
        let dir = temp_registry("vartgt");
        assert_eq!(super::variant_modified(&dir, "debug"), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_registry_fills_variant_dates() {
        let dir = temp_registry("vardates");
        let proj = dir.join("proj");
        fs::create_dir_all(proj.join("target/debug")).unwrap();
        fs::write(proj.join("target/debug/art.rlib"), b"x").unwrap();

        let path = dir.join("opencode/projects.json");
        load_registry(&path).unwrap();
        let json = format!(
            r#"{{"projects":{{"proj":{{"name":"proj","path":"{}","description":"d","status":"","version":"1.0.0"}}}}}}"#,
            proj.display()
        );
        fs::write(&path, json).unwrap();

        let loaded = load_registry(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_ne!(loaded[0].debug_modified, "jamais compilée");
        assert_eq!(loaded[0].release_modified, "jamais compilée");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn variant_modified_formats_latest_target_mtime() {
        let dir = temp_registry("vartgt2");
        let debug = dir.join("target/debug");
        fs::create_dir_all(&debug).unwrap();
        fs::write(debug.join("old.rlib"), b"x").unwrap();
        fs::write(debug.join("new.rlib"), b"x").unwrap();
        let new_mtime = fs::metadata(debug.join("new.rlib")).unwrap().modified().unwrap();
        let label = super::variant_modified(&dir, "debug").expect("date debug");
        let expected = super::format_mod_time(new_mtime);
        assert_eq!(label, expected, "doit refléter le mtime le plus récent");
        assert_eq!(super::variant_modified(&dir, "release"), None);
        fs::remove_dir_all(&dir).unwrap();
    }
  }