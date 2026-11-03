use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use tempfile::TempDir;
use walkdir::WalkDir;

use crate::epub;
use crate::model::Book;

pub struct StagedEpub {
    _directory: TempDir,
    path: PathBuf,
}

impl StagedEpub {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub fn download(url: &str) -> Result<StagedEpub> {
    download_with(OsStr::new("fanficfare"), url)
}

fn download_with(program: &OsStr, url: &str) -> Result<StagedEpub> {
    validate_url(url)?;
    let directory = tempfile::tempdir().context("cannot create a private download directory")?;
    run_fanficfare(
        program,
        &["--non-interactive", "--format", "epub", url],
        directory.path(),
    )?;
    let path = find_epub(directory.path())?;
    validate_epub(&path)?;
    let metadata = epub::read_metadata(&path)?;
    if !metadata
        .identifiers
        .iter()
        .any(|identifier| identifier.source_url().is_some())
    {
        bail!("FanFicFare output has no recognizable story URL in its EPUB metadata");
    }
    Ok(StagedEpub {
        _directory: directory,
        path,
    })
}

pub fn update(book: &Book, epub_path: &Path) -> Result<StagedEpub> {
    update_with(OsStr::new("fanficfare"), book, epub_path)
}

fn update_with(program: &OsStr, book: &Book, epub_path: &Path) -> Result<StagedEpub> {
    let url = book
        .source_url()
        .context("this book has no FanFicFare source URL and cannot be updated")?;
    let directory = tempfile::tempdir().context("cannot create a private update directory")?;
    let staged_path = directory.path().join("update.epub");
    fs::copy(epub_path, &staged_path)
        .with_context(|| format!("cannot stage {}", epub_path.display()))?;
    run_fanficfare(
        program,
        &[
            "--non-interactive",
            "--update-epub",
            staged_path
                .to_str()
                .context("staged path is not valid UTF-8")?,
        ],
        directory.path(),
    )?;
    validate_epub(&staged_path)?;
    let updated = epub::read_metadata(&staged_path)?;
    let updated_url = updated
        .identifiers
        .iter()
        .find_map(crate::model::Identifier::source_url);
    if updated_url != Some(url) {
        bail!("FanFicFare's updated EPUB does not match the stored story URL");
    }
    Ok(StagedEpub {
        _directory: directory,
        path: staged_path,
    })
}

fn validate_url(url: &str) -> Result<()> {
    if url.starts_with("https://") || url.starts_with("http://") {
        Ok(())
    } else {
        bail!("FanFicFare requires an HTTP or HTTPS story URL");
    }
}

fn run_fanficfare(program: &OsStr, arguments: &[&str], directory: &Path) -> Result<()> {
    let output = Command::new(program)
        .args(arguments)
        .current_dir(directory)
        .output()
        .with_context(|| {
            format!(
                "cannot run FanFicFare (`{}`); install FanFicFare and ensure `fanficfare` is on PATH",
                program.to_string_lossy()
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let details = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        bail!(
            "FanFicFare failed with {}{}",
            output.status,
            if details.is_empty() {
                String::new()
            } else {
                format!(": {details}")
            }
        );
    }
    Ok(())
}

fn find_epub(directory: &Path) -> Result<PathBuf> {
    let mut epubs = Vec::new();
    for entry in WalkDir::new(directory).follow_links(false) {
        let entry = entry.context("cannot inspect FanFicFare download output")?;
        if entry.file_type().is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("epub"))
        {
            epubs.push(entry.into_path());
        }
    }
    epubs.sort();
    match epubs.as_slice() {
        [path] => Ok(path.clone()),
        [] => bail!("FanFicFare completed without producing an EPUB file"),
        _ => bail!("FanFicFare produced more than one EPUB; refusing an ambiguous import"),
    }
}

fn validate_epub(path: &Path) -> Result<()> {
    if !crate::util::is_epub_file(path) {
        bail!("FanFicFare output is not an EPUB file");
    }
    epub::read_metadata(path)
        .context("FanFicFare output is not a valid EPUB")
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{EpubSpec, write_epub};
    use crate::library::{AddOutcome, Library};
    use crate::model::Identifier;

    #[test]
    fn recognizes_supported_url_identifier_shapes() {
        assert_eq!(
            Identifier {
                scheme: Some("URL".to_string()),
                value: "https://archive.example/story/1".to_string(),
            }
            .source_url(),
            Some("https://archive.example/story/1")
        );
        assert_eq!(
            Identifier {
                scheme: None,
                value: "URL:https://archive.example/story/1".to_string(),
            }
            .source_url(),
            Some("https://archive.example/story/1")
        );
        assert_eq!(
            Identifier {
                scheme: Some("ISBN".to_string()),
                value: "9780000000000".to_string(),
            }
            .source_url(),
            None
        );
    }

    #[test]
    fn rejects_non_http_story_urls() {
        assert!(validate_url("https://archive.example/story/1").is_ok());
        assert!(validate_url("file:///tmp/story").is_err());
        assert!(validate_url("--update-epub").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn stages_downloads_and_updates_through_the_expected_cli() {
        use std::os::unix::fs::PermissionsExt;

        fn script(path: &Path, contents: &str) {
            fs::write(path, format!("#!/bin/sh\n{contents}\n")).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }

        fn quote(path: &Path) -> String {
            format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
        }

        let temporary = tempfile::tempdir().unwrap();
        let url = "https://archive.example/story/11";
        let fixture = temporary.path().join("source.epub");
        write_epub(
            &fixture,
            &EpubSpec {
                source_url: Some(url.to_string()),
                ..EpubSpec::new("Test Fic", "An Author")
            },
        )
        .unwrap();
        let downloader = temporary.path().join("download");
        script(
            &downloader,
            &format!(
                "[ \"$1\" = \"--non-interactive\" ] && [ \"$2\" = \"--format\" ] && [ \"$3\" = \"epub\" ] || exit 11\ncp -- {} story.epub",
                quote(&fixture)
            ),
        );
        let staged = download_with(downloader.as_os_str(), url).unwrap();
        assert_eq!(
            epub::read_metadata(staged.path())
                .unwrap()
                .identifiers
                .iter()
                .find_map(Identifier::source_url),
            Some(url)
        );

        let mut library = Library::open(temporary.path().join("library")).unwrap();
        let book = match library.add_epub(staged.path()).unwrap() {
            AddOutcome::Added(book) => book,
            AddOutcome::Duplicate(_) => panic!("test EPUB was already in the library"),
        };
        let updater = temporary.path().join("update");
        script(
            &updater,
            "[ \"$1\" = \"--non-interactive\" ] && [ \"$2\" = \"--update-epub\" ] && [ -f \"$3\" ] || exit 12",
        );
        let updated = update_with(updater.as_os_str(), &book, &library.book_path(&book)).unwrap();
        assert_eq!(
            epub::read_metadata(updated.path())
                .unwrap()
                .identifiers
                .iter()
                .find_map(Identifier::source_url),
            Some(url)
        );
    }

    #[test]
    fn missing_executable_has_installation_guidance() {
        let error = run_fanficfare(
            OsStr::new("/no/such/fanficfare-command"),
            &[],
            Path::new("/"),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("install FanFicFare"));
    }
}
