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