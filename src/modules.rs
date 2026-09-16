use std::{
    fs,
    path::{Path, PathBuf},
};

const IGNORED: &[&str] = &[
    ".git", "node_modules", "target", "dist", "build", ".next", ".nuxt",
    ".venv", "venv", "vendor", ".env", ".idea", ".vscode", "__pycache__",
    ".cache", "coverage", "out",
];

pub(crate) fn list_modules(project: &Path) -> Vec<PathBuf>
  {
    let mut modules: Vec<PathBuf> = fs::read_dir(project)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter(|e| {
            let file_name = e.file_name();
            let name = file_name.to_string_lossy();
            !IGNORED.contains(&name.as_ref())
        })
        .map(|e| e.path())
        .collect();
    modules.sort();
    modules
  }

#[cfg(test)]
mod tests
  {
    use std::{fs, path::PathBuf};

    use super::list_modules;

    fn temp_project(name: &str) -> PathBuf
      {
        let dir = std::env::temp_dir()
            .join(format!("op-modules-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&dir);
        for sub in [".git", "node_modules", "target", "src", "api"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        dir
      }

    fn names(mods: &[PathBuf]) -> Vec<String>
      {
        mods.iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect()
      }

    #[test]
    fn filters_ignored_dirs_and_sorts() {
        let proj = temp_project("proj");
        let mods = list_modules(&proj);
        assert_eq!(names(&mods), vec!["api", "src"]);
        let _ = fs::remove_dir_all(&proj);
    }

    #[test]
    fn empty_when_only_ignored_dirs() {
        let dir = std::env::temp_dir()
            .join(format!("op-modules-{}-empty", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for sub in [".git", "node_modules", "target"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        assert!(list_modules(&dir).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
  }