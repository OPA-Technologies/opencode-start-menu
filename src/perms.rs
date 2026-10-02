use std::{io, path::Path};

#[cfg(unix)]
use std::fs;

// Sur Unix, restreint les permissions à l'utilisateur seul. Sur les autres
// plateformes (Windows, ...), pas d'ACL unix : no-op, les droits par défaut
// du compte s'appliquent.
#[cfg(unix)]
pub(crate) fn private_file(path: &Path) -> io::Result<()>
  {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
  }

#[cfg(not(unix))]
pub(crate) fn private_file(_path: &Path) -> io::Result<()>
  {
    Ok(())
  }

#[cfg(unix)]
pub(crate) fn private_dir(path: &Path) -> io::Result<()>
  {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
  }

#[cfg(not(unix))]
pub(crate) fn private_dir(_path: &Path) -> io::Result<()>
  {
    Ok(())
  }

/// Crée `path` déjà restreint à l'utilisateur seul, en une seule opération.
///
/// `private_file` appliquée après coup laisse une fenêtre pendant laquelle le
/// fichier existe avec les droits de l'umask. Ici le mode est porté par
/// l'appel `open`, donc le fichier n'est jamais visible que par son
/// propriétaire. Sur les plateformes sans ACL unix, l'écriture se fait
/// normalement.
#[cfg(unix)]
pub(crate) fn create_private(path: &Path, contenu: &[u8]) -> io::Result<()>
  {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut fichier = fs::OpenOptions::new()
      .write(true)
      .create(true)
      .truncate(true)
      .mode(0o600)
      .open(path)?;
    fichier.write_all(contenu)
  }

#[cfg(not(unix))]
pub(crate) fn create_private(path: &Path, contenu: &[u8]) -> io::Result<()>
  {
    fs::write(path, contenu)
  }