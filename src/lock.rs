use anyhow::{anyhow, Result};
use fs2::FileExt;
use std::{
    fs::{self, File},
    io::ErrorKind,
    path::{Path, PathBuf},
};

use crate::perms;

pub(crate) struct LockGuard {
    _file: File,
}

fn fnv1a(input: &[u8]) -> u64
  {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in input
      {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
      }
    hash
  }

fn locks_dir() -> PathBuf
  {
    let base = dirs::state_dir()
        .unwrap_or_else(|| dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")));
    base.join("opencode/locks")
  }

// Verrou exclusive par répertoire : tenir le LockGuard pendant toute la
// durée de la session opencode. Deux répertoires différents (ex. deux
// modules du même projet) obtiennent chacun leur propre verrou et peuvent
// tourner en parallèle.
pub(crate) fn acquire(dir: &Path) -> Result<LockGuard>
  {
    let abs = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let digest = format!("{:016x}", fnv1a(abs.to_string_lossy().as_bytes()));

    let lock_dir = locks_dir();
    fs::create_dir_all(&lock_dir)?;
    let _ = perms::private_dir(&lock_dir);

    let path = lock_dir.join(format!("{digest}.lock"));
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .read(true)
        .open(&path)?;
    let _ = perms::private_file(&path);

    match file.try_lock_exclusive()
      {
        Ok(()) => Ok(LockGuard { _file: file }),
        Err(e) if e.kind() == ErrorKind::WouldBlock => {
          Err(anyhow!(
            "une session opencode est déjà active dans ce répertoire"
          ))
        }
        Err(e) => Err(e.into()),
      }
  }

#[cfg(test)]
mod tests
  {
    use std::path::PathBuf;

    use super::acquire;

    fn temp(name: &str) -> PathBuf
      {
        let dir = std::env::temp_dir()
            .join(format!("op-lock-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
      }

    #[test]
    fn locks_are_per_directory() {
        let a = temp("a");
        let b = temp("b");
        let _la = acquire(&a).expect("premier verrou");
        let _lb = acquire(&b).expect("verrou différent autorisé");
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn second_lock_same_dir_blocked_then_released() {
        let dir = temp("same");
        let la = acquire(&dir).expect("premier verrou");
        assert!(acquire(&dir).is_err(), "double saisie doit être bloquée");
        drop(la);
        let _re = acquire(&dir).expect("verrou relâché permet de réouvrir");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_dir_via_different_spellings() {
        let dir = temp("spell");
        let _la = acquire(&dir).expect("premier verrou");
        let alias = dir.join("..").join(dir.file_name().unwrap());
        assert!(acquire(&alias).is_err(), "le même répertoire via ../ doit rester verrouillé");
        let _ = std::fs::remove_dir_all(&dir);
    }
  }