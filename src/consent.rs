use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// File that records a one-time "I am an adult" confirmation for FanFicFare
/// downloads (notably AO3's "are you 18?" check). Stored globally so one
/// confirmation covers every library.
pub fn consent_file_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("CLIBRARY_ADULT_CONSENT_FILE")
        && !path.trim().is_empty()
    {
        return Some(PathBuf::from(path));
    }
    dirs::config_dir().map(|dir| dir.join("clibrary").join("adult_consent"))
}

/// Whether adult consent has been remembered.
pub fn is_adult_consented() -> bool {
    let Some(path) = consent_file_path() else {
        return false;
    };
    is_adult_consented_at(&path)
}

fn is_adult_consented_at(path: &Path) -> bool {
    match fs::read_to_string(path) {
        Ok(contents) => contents.trim().eq_ignore_ascii_case("true"),
        Err(_) => false,
    }
}

/// Remember adult consent for future downloads.
pub fn set_adult_consented() -> Result<()> {
    let Some(path) = consent_file_path() else {
        bail!("cannot determine the config directory; set CLIBRARY_ADULT_CONSENT_FILE");
    };
    set_adult_consented_at(&path)
}

fn set_adult_consented_at(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        crate::util::ensure_private_dir(parent)?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("cannot write {}", path.display()))?;
    use std::io::Write;
    file.write_all(b"true\n")
        .with_context(|| format!("cannot write {}", path.display()))?;
    file.flush()?;
    file.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).ok();
    }
    if let Some(parent) = path.parent() {
        crate::util::sync_dir(parent);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_consent_at_an_explicit_path() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("nested").join("adult_consent");
        assert!(!is_adult_consented_at(&path));
        set_adult_consented_at(&path).unwrap();
        assert!(is_adult_consented_at(&path));
        assert_eq!(fs::read_to_string(&path).unwrap(), "true\n");
    }

    #[test]
    fn ignores_unrelated_consent_file_contents() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("adult_consent");
        fs::write(&path, b"no\n").unwrap();
        assert!(!is_adult_consented_at(&path));
    }

    #[cfg(unix)]
    #[test]
    fn consent_file_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("adult_consent");
        set_adult_consented_at(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
