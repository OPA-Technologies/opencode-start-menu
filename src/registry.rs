use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
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
                modified,
            }
        })
        .collect::<Vec<ProjectEntry>>();

    Ok(projects)
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