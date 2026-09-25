use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use crate::perms;

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
    #[serde(default = "default_true")]
    active: bool,
}

fn default_true() -> bool {
    true
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
            }
        })
        .collect::<Vec<ProjectEntry>>();

    Ok(projects)
}

pub(crate) fn load_usage(db_path: &Path) -> HashMap<PathBuf, (u64, f64, Option<(String, String)>)> {
    let mut usage = HashMap::new();
    let Ok(conn) = Connection::open(db_path) else {
        return usage;
    };
    let mut stmt = match conn.prepare(
        "SELECT directory, COALESCE(cost,0), \
         COALESCE(tokens_input,0) + COALESCE(tokens_output,0) + COALESCE(tokens_reasoning,0), \
         model, agent, time_created \
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
        ))
    });
    match rows {
        Ok(iter) => {
            for r in iter.flatten() {
                let (dir, cost, tokens, model, agent, _t) = r;
                let e = usage
                    .entry(PathBuf::from(dir))
                    .or_insert((0u64, 0.0f64, None::<(String, String)>));
                e.0 += tokens;
                e.1 += cost;
                if e.2.is_none() {
                    let model_id = model.and_then(|m| {
                        serde_json::from_str::<serde_json::Value>(&m)
                            .ok()
                            .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()))
                    });
                    let agent_cap = agent.map(|a| capitalize(&a));
                    e.2 = match (model_id, agent_cap) {
                        (Some(m), Some(a)) => Some((m, a)),
                        (Some(m), None) => Some((m, String::new())),
                        _ => None,
                    };
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

fn last_modified(dir: &Path) -> Option<String> {
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
        .map(|t| {
            let dt: chrono::DateTime<chrono::Local> = t.into();
            dt.format("%d/%m/%Y %H:%M").to_string()
        })
}

#[cfg(test)]
mod tests
  {
    use std::{fs, path::PathBuf};

    use super::{load_registry, save_registry, ProjectEntry};

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
  }