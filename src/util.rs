use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

pub const EPUB_EXTENSION: &str = "epub";

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn format_timestamp(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "-".to_string())
}

pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Extract a four-digit year from a metadata date such as `1937`,
/// `1937-09-21`, `1937-09-21T00:00:00Z` or `21/09/1937`.
pub fn parse_year(text: &str) -> Option<i32> {
    text.split(|c: char| !c.is_ascii_digit())
        .find(|part| part.len() == 4)
        .and_then(|part| part.parse::<i32>().ok())
        .filter(|year| (1..=2999).contains(year))
}

pub fn is_epub_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case(EPUB_EXTENSION))
}

/// Make a single path component safe on every platform we support.
pub fn sanitize_component(input: &str, fallback: &str, max_chars: usize) -> String {
    const MAX_BYTES: usize = 200;
    let mut result = String::with_capacity(input.len());
    let mut last_was_space = false;
    for ch in input.chars() {
        let ch = match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => ' ',
            c => c,
        };
        if ch.is_whitespace() {
            if !last_was_space {
                result.push(' ');
                last_was_space = true;
            }
        } else {
            result.push(ch);
            last_was_space = false;
        }
    }
    let trimmed = result.trim().trim_matches('.');
    let mut cleaned = String::new();
    for ch in trimmed.chars().take(max_chars) {
        if cleaned.len() + ch.len_utf8() > MAX_BYTES {
            break;
        }
        cleaned.push(ch);
    }
    cleaned = cleaned.trim_end().trim_matches('.').to_string();
    if cleaned.is_empty() {
        cleaned = fallback.to_string();
    }
    // Windows reserves these device names, with or without an extension.
    let stem = cleaned
        .split('.')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem[3..].parse::<u8>().is_ok_and(|n| (1..=9).contains(&n)));
    if reserved {
        cleaned = format!("_{cleaned}");
    }
    cleaned
}

/// Turn a description that may contain escaped or literal HTML into plain text.
pub fn html_to_text(input: &str) -> String {
    let mut text = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' if !in_tag => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                text.push(' ');
            }
            _ if in_tag => {}
            _ => text.push(ch),
        }
    }
    let decoded = decode_entities(&text);
    collapse_whitespace(&decoded)
}

/// Decode a named HTML entity such as `nbsp` or `mdash` (without the leading
/// `&` and trailing `;`). Numeric entities are not handled here.
pub fn decode_named_entity(entity: &str) -> Option<char> {
    Some(match entity {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" | "#39" => '\'',
        "nbsp" => ' ',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "rsquo" => '’',
        "lsquo" => '‘',
        "ldquo" => '“',
        "rdquo" => '”',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "deg" => '°',
        "middot" => '·',
        "bull" => '•',
        "times" => '×',
        "divide" => '÷',
        "laquo" => '«',
        "raquo" => '»',
        "shy" => '\u{00AD}',
        _ => return None,
    })
}

fn decode_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 1..];
        if let Some(end) = after.find(';') {
            let entity = &after[..end];
            let decoded = decode_named_entity(entity).or_else(|| {
                entity.strip_prefix('#').and_then(|number| {
                    let number = number.strip_prefix(['x', 'X']).unwrap_or(number);
                    let code = if entity.starts_with("#x") || entity.starts_with("#X") {
                        u32::from_str_radix(number, 16).ok()
                    } else {
                        number.parse::<u32>().ok()
                    };
                    code.and_then(char::from_u32)
                })
            });
            match decoded {
                Some(ch) => {
                    out.push(ch);
                    rest = &after[end + 1..];
                }
                None => {
                    out.push('&');
                    rest = after;
                }
            }
        } else {
            out.push('&');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

fn collapse_whitespace(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_was_space = false;
    for ch in input.trim().chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    out
}

pub fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

pub fn hash_file(path: &Path) -> Result<(String, u64)> {
    let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let read = reader
            .read(&mut buffer)
            .with_context(|| format!("cannot read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size += read as u64;
    }
    Ok((hex::encode(hasher.finalize()), size))
}

pub fn ensure_private_dir(path: &Path) -> Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    if path.exists() {
        bail!("{} exists but is not a directory", path.display());
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .with_context(|| format!("cannot create directory {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).ok();
    }
    Ok(())
}

pub fn create_private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .with_context(|| format!("cannot create {}", path.display()))
}

pub fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = BufWriter::new(create_private_file(path)?);
    file.write_all(bytes)
        .with_context(|| format!("cannot write {}", path.display()))?;
    file.flush()?;
    file.get_ref().sync_all()?;
    Ok(())
}

/// Copy a file, verify the copy against the source hash, then move it into
/// place. The temporary file is removed if anything fails.
pub fn copy_verified(
    source: &Path,
    temporary: &Path,
    destination: &Path,
    expected_hash: &str,
) -> Result<u64> {
    let result = (|| -> Result<u64> {
        let input =
            File::open(source).with_context(|| format!("cannot open {}", source.display()))?;
        let mut reader = BufReader::new(input);
        let mut writer = BufWriter::new(create_private_file(temporary)?);
        io::copy(&mut reader, &mut writer)
            .with_context(|| format!("cannot copy {}", source.display()))?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);

        let (hash, size) = hash_file(temporary)?;
        if hash != expected_hash {
            bail!("verification of the copied file failed");
        }
        fs::rename(temporary, destination).with_context(|| {
            format!(
                "cannot move copied book into place at {}",
                destination.display()
            )
        })?;
        sync_dir(destination.parent().unwrap_or_else(|| Path::new(".")));
        Ok(size)
    })();

    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

pub fn sync_dir(path: &Path) {
    #[cfg(unix)]
    if let Ok(dir) = File::open(path) {
        let _ = dir.sync_all();
    }
}

pub fn remove_file_if_exists(path: &Path) {
    if path.exists() {
        let _ = fs::remove_file(path);
    }
}

/// Remove a directory only when it is safely inside `allowed_root`.
pub fn remove_dir_inside(allowed_root: &Path, directory: &Path) -> Result<bool> {
    if !is_inside(allowed_root, directory) {
        bail!(
            "refusing to delete {} because it is outside {}",
            directory.display(),
            allowed_root.display()
        );
    }
    if !directory.exists() {
        return Ok(false);
    }
    fs::remove_dir_all(directory)
        .with_context(|| format!("cannot delete {}", directory.display()))?;
    Ok(true)
}

/// Lexical containment check (both paths are expected to be normalized).
pub fn is_inside(root: &Path, path: &Path) -> bool {
    path != root && path.starts_with(root)
}

/// Return the path inside `root` for a stored, validated relative path.
pub fn library_path(root: &Path, relative: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for component in relative.split('/') {
        path.push(component);
    }
    path
}

/// Validate a path from the index so a corrupted or edited index can never
/// point outside the library.
pub fn validate_relative_path(relative: &str, expected_first: &str) -> Result<()> {
    if relative.is_empty() || relative.contains('\\') || Path::new(relative).is_absolute() {
        bail!("invalid library path {relative:?}");
    }
    let components: Vec<&str> = relative.split('/').collect();
    if components.first() != Some(&expected_first) {
        bail!("library path {relative:?} does not start with {expected_first}/");
    }
    if components
        .iter()
        .any(|component| component.is_empty() || *component == "." || *component == "..")
    {
        bail!("library path {relative:?} contains an unsafe component");
    }
    Ok(())
}

pub fn open_in_viewer(path: &Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    let attempts: &[(&str, &[&str])] = &[("xdg-open", &[]), ("gio", &["open"])];
    #[cfg(target_os = "macos")]
    let attempts: &[(&str, &[&str])] = &[("open", &[])];
    #[cfg(target_os = "windows")]
    let attempts: &[(&str, &[&str])] = &[("cmd", &["/C", "start", ""])];

    let mut last_error = None;
    for (program, args) in attempts {
        let mut command = Command::new(program);
        command
            .args(*args)
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match command.spawn() {
            Ok(_) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
    }
    match last_error {
        Some(error) => Err(error).context("no EPUB viewer launcher is available"),
        None => bail!("no EPUB viewer launcher is available"),
    }
}

pub fn percent_decode(input: &str) -> String {
    percent_encoding::percent_decode_str(input)
        .decode_utf8_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_path_components() {
        assert_eq!(
            sanitize_component("The Hobbit", "Untitled", 80),
            "The Hobbit"
        );
        assert_eq!(sanitize_component("a/b:c*?", "Untitled", 80), "a-b-c--");
        assert_eq!(
            sanitize_component("../../etc/passwd", "Untitled", 80),
            "-..-etc-passwd"
        );
        assert_eq!(sanitize_component("   ", "Untitled", 80), "Untitled");
        assert_eq!(sanitize_component("CON", "Untitled", 80), "_CON");
        assert_eq!(sanitize_component("com1.txt", "Untitled", 80), "_com1.txt");
        assert_eq!(sanitize_component(".hidden", "Untitled", 80), "hidden");
        assert_eq!(sanitize_component("abcdef", "x", 3), "abc");
        let long_cjk = "漢".repeat(100);
        let cleaned = sanitize_component(&long_cjk, "Untitled", 100);
        assert!(cleaned.len() <= 200);
        assert!(cleaned.chars().count() > 0);
    }

    #[test]
    fn parses_years() {
        assert_eq!(parse_year("1937"), Some(1937));
        assert_eq!(parse_year("1937-09-21"), Some(1937));
        assert_eq!(parse_year("1937-09-21T00:00:00+00:00"), Some(1937));
        assert_eq!(parse_year("21/09/1937"), Some(1937));
        assert_eq!(parse_year("unknown"), None);
    }

    #[test]
    fn converts_html_descriptions() {
        assert_eq!(
            html_to_text("<p>Hello <b>world</b> &amp; friends</p>"),
            "Hello world & friends"
        );
        assert_eq!(html_to_text("A&nbsp;B&#8212;C"), "A B—C");
        assert_eq!(html_to_text("no markup"), "no markup");
    }

    #[test]
    fn rejects_unsafe_relative_paths() {
        assert!(validate_relative_path("books/A/book.epub", "books").is_ok());
        assert!(validate_relative_path("../outside/book.epub", "books").is_err());
        assert!(validate_relative_path("books/../../x", "books").is_err());
        assert!(validate_relative_path("/etc/passwd", "books").is_err());
        assert!(validate_relative_path("covers/a.jpg", "books").is_err());
        assert!(validate_relative_path("books/a\\..\\b", "books").is_err());
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1536), "1.5 KiB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MiB");
    }
}
