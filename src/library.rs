use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fs2::FileExt;

use crate::epub;
use crate::model::{Book, INDEX_VERSION, Index, ReaderSettings, ReadingProgress};
use crate::util;

pub struct Library {
    root: PathBuf,
    index: Index,
}

#[derive(Debug)]
pub enum AddOutcome {
    Added(Book),
    Duplicate(Book),
}

#[derive(Debug)]
pub struct Removal {
    pub book: Book,
    pub files_removed: bool,
    pub warning: Option<String>,
}

#[derive(Debug)]
pub struct Export {
    pub book: Book,
    pub path: PathBuf,
}

impl Library {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        util::ensure_private_dir(&root)?;
        let root = fs::canonicalize(&root)
            .with_context(|| format!("cannot resolve library path {}", root.display()))?;
        util::ensure_private_dir(&root.join("books"))?;
        let books_dir = fs::canonicalize(root.join("books"))
            .with_context(|| format!("cannot resolve {}", root.join("books").display()))?;
        if !books_dir.starts_with(&root) {
            bail!(
                "the library's books directory resolves outside the library: {}",
                books_dir.display()
            );
        }
        let mut library = Self {
            root,
            index: Index::default(),
        };
        library.reload()?;
        Ok(library)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn books(&self) -> &[Book] {
        &self.index.books
    }

    pub fn reader_settings(&self) -> &ReaderSettings {
        &self.index.reader
    }

    pub fn index_path(&self) -> PathBuf {
        self.root.join("index.json")
    }

    pub fn books_dir(&self) -> PathBuf {
        self.root.join("books")
    }

    pub fn book_path(&self, book: &Book) -> PathBuf {
        util::library_path(&self.root, &book.epub_path)
    }

    pub fn book_dir(&self, book: &Book) -> PathBuf {
        self.book_path(book)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.books_dir())
    }

    pub fn exists(&self, book: &Book) -> bool {
        self.book_path(book).is_file()
    }

    pub fn reload(&mut self) -> Result<()> {
        let path = self.index_path();
        if !path.exists() {
            self.index = Index::default();
            return Ok(());
        }
        let bytes = fs::read(&path)
            .with_context(|| format!("cannot read library index {}", path.display()))?;
        let index: Index = serde_json::from_slice(&bytes)
            .with_context(|| format!("library index {} is not valid JSON", path.display()))?;
        validate_index(&index)?;
        self.index = index;
        Ok(())
    }

    fn save(&self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&self.index).context("cannot serialize index")?;
        let temporary = self
            .root
            .join(format!(".index.json.tmp-{}", std::process::id()));
        util::remove_file_if_exists(&temporary);
        let result = (|| -> Result<()> {
            let mut content = bytes.clone();
            content.push(b'\n');
            util::write_private_file(&temporary, &content)?;
            fs::rename(&temporary, self.index_path())
                .with_context(|| format!("cannot replace {}", self.index_path().display()))?;
            util::sync_dir(&self.root);
            Ok(())
        })();
        if result.is_err() {
            util::remove_file_if_exists(&temporary);
        }
        result
    }

    fn lock(&self) -> Result<File> {
        let path = self.root.join("library.lock");
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .with_context(|| format!("cannot open lock file {}", path.display()))?;
        file.lock_exclusive()
            .with_context(|| format!("cannot lock library at {}", self.root.display()))?;
        Ok(file)
    }

    /// Import a single EPUB. The file is copied (never modified in place) and
    /// the copy is verified against the source hash before it is committed.
    pub fn add_epub(&mut self, source: &Path) -> Result<AddOutcome> {
        if !util::is_epub_file(source) {
            bail!("{} is not an EPUB file", source.display());
        }
        let source = fs::canonicalize(source)
            .with_context(|| format!("cannot resolve {}", source.display()))?;
        if !source.is_file() {
            bail!("{} is not a regular file", source.display());
        }
        if source.starts_with(self.books_dir()) {
            bail!("refusing to add a file that is already inside the library");
        }

        let (hash, size) = util::hash_file(&source)?;
        let parsed = epub::read_metadata(&source)?;
        let fallback_title = source
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("Untitled");
        let title = parsed
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .unwrap_or(fallback_title)
            .to_string();
        let author = parsed
            .authors
            .first()
            .map(String::as_str)
            .unwrap_or("Unknown Author");

        let _lock = self.lock()?;
        self.reload()?;
        if let Some(existing) = self.index.books.iter().find(|book| book.hash == hash) {
            return Ok(AddOutcome::Duplicate(existing.clone()));
        }

        let title_dir = util::sanitize_component(&title, "Untitled", 100);
        let author_dir = util::sanitize_component(author, "Unknown Author", 80);
        let base = format!("books/{author_dir}/{title_dir}");
        let mut relative_dir = base.clone();
        if self.root.join(&relative_dir).exists() {
            relative_dir = format!("{base} ({})", &hash[..8]);
        }
        if self.root.join(&relative_dir).exists() {
            relative_dir = format!("{base} ({hash})");
        }
        let book_dir = self.root.join(&relative_dir);
        if book_dir.exists() {
            bail!(
                "destination {} already exists; refusing to overwrite",
                book_dir.display()
            );
        }

        util::ensure_private_dir(&book_dir)?;
        let relative_epub = format!("{relative_dir}/book.epub");
        let destination = self.root.join(&relative_epub);
        let temporary = book_dir.join(format!(".book.epub.tmp-{}", std::process::id()));

        let copied_size = match util::copy_verified(&source, &temporary, &destination, &hash) {
            Ok(size) => size,
            Err(error) => {
                let _ = fs::remove_dir_all(&book_dir);
                return Err(error);
            }
        };
        if copied_size != size {
            let _ = fs::remove_dir_all(&book_dir);
            bail!("the copied EPUB did not match the source size");
        }

        let cover_path = parsed.cover.and_then(|cover| {
            let name = format!("cover.{}", cover.extension);
            let relative = format!("{relative_dir}/{name}");
            match util::write_private_file(&self.root.join(&relative), &cover.bytes) {
                Ok(()) => Some(relative),
                Err(_) => None,
            }
        });

        let book = Book {
            id: hash[..12].to_string(),
            hash,
            title,
            author_sort: parsed.author_sort,
            authors: parsed.authors,
            series: parsed.series,
            series_index: parsed.series_index,
            publisher: parsed.publisher,
            published: parsed.published,
            language: parsed.language,
            description: parsed.description,
            subjects: parsed.subjects,
            identifiers: parsed.identifiers,
            epub_path: relative_epub,
            cover_path,
            added_at: util::now_unix(),
            last_opened_at: None,
            size,
            reading_progress: None,
        };

        self.index.books.push(book.clone());
        if let Err(error) = self.save() {
            self.index.books.pop();
            let _ = fs::remove_dir_all(&book_dir);
            return Err(error.context("could not update the library index"));
        }

        Ok(AddOutcome::Added(book))
    }

    pub fn find_by_hash(&self, hash: &str) -> Option<&Book> {
        self.index.books.iter().find(|book| book.hash == hash)
    }

    /// Record where the reader left off in a book.
    pub fn set_reading_progress(&mut self, id: &str, mut progress: ReadingProgress) -> Result<()> {
        progress.updated_at = util::now_unix();
        let _lock = self.lock()?;
        self.reload()?;
        let position = self
            .index
            .books
            .iter()
            .position(|book| book.id == id)
            .with_context(|| format!("book {id} is not in the library"))?;
        let previous = self.index.books[position]
            .reading_progress
            .replace(progress);
        if let Err(error) = self.save() {
            self.index.books[position].reading_progress = previous;
            return Err(error.context("could not save reading progress"));
        }
        Ok(())
    }

    /// Record that a book was opened, either in the in-app reader or a
    /// system viewer.
    pub fn record_opened(&mut self, id: &str) -> Result<()> {
        let _lock = self.lock()?;
        self.reload()?;
        let position = self
            .index
            .books
            .iter()
            .position(|book| book.id == id)
            .with_context(|| format!("book {id} is not in the library"))?;
        let previous = self.index.books[position]
            .last_opened_at
            .replace(util::now_unix());
        if let Err(error) = self.save() {
            self.index.books[position].last_opened_at = previous;
            return Err(error.context("could not save the open time"));
        }
        Ok(())
    }

    /// Record the reader preferences shared by every book.
    pub fn set_reader_settings(&mut self, settings: ReaderSettings) -> Result<()> {
        let _lock = self.lock()?;
        self.reload()?;
        let previous = std::mem::replace(&mut self.index.reader, settings);
        if let Err(error) = self.save() {
            self.index.reader = previous;
            return Err(error.context("could not save reader settings"));
        }
        Ok(())
    }

    /// Resolve a query as an exact ID, an ID prefix, or a metadata search.
    pub fn resolve(&self, query: &str) -> Result<&Book> {
        let query = query.trim();
        if query.is_empty() {
            bail!("please provide a book ID or search term");
        }
        if let Some(book) = self.index.books.iter().find(|book| book.id == query) {
            return Ok(book);
        }

        let lowered = query.to_ascii_lowercase();
        if query.len() >= 4 && query.chars().all(|ch| ch.is_ascii_hexdigit()) {
            let matches: Vec<&Book> = self
                .index
                .books
                .iter()
                .filter(|book| book.id.starts_with(&lowered))
                .collect();
            match matches.len() {
                1 => return Ok(matches[0]),
                0 => {}
                _ => return Err(ambiguous(query, &matches)),
            }
        }

        let matches: Vec<&Book> = self
            .index
            .books
            .iter()
            .filter(|book| book.matches(query))
            .collect();
        match matches.len() {
            1 => Ok(matches[0]),
            0 => bail!("no book matches {query:?}"),
            _ => Err(ambiguous(query, &matches)),
        }
    }

    pub fn remove(&mut self, id: &str, keep_files: bool) -> Result<Removal> {
        let _lock = self.lock()?;
        self.reload()?;
        let position = self
            .index
            .books
            .iter()
            .position(|book| book.id == id)
            .with_context(|| format!("book {id} is not in the library"))?;
        let book = self.index.books.remove(position);
        if let Err(error) = self.save() {
            self.index.books.insert(position, book);
            return Err(error.context("could not update the library index"));
        }

        let mut warning = None;
        let mut files_removed = false;
        if keep_files {
            warning = Some(format!(
                "files were left in {}",
                self.book_dir(&book).display()
            ));
        } else {
            let book_dir = self.book_dir(&book);
            match util::remove_dir_inside(&self.books_dir(), &book_dir) {
                Ok(removed) => files_removed = removed,
                Err(error) => {
                    warning = Some(format!("{error:#}"));
                }
            }
        }

        Ok(Removal {
            book,
            files_removed,
            warning,
        })
    }

    pub fn export(&self, query: &str, destination: &Path) -> Result<Export> {
        let book = self.resolve(query)?.clone();
        let source = self.book_path(&book);
        if !source.is_file() {
            bail!(
                "the EPUB file for this book is missing: {}",
                source.display()
            );
        }
        util::ensure_private_dir(destination)?;
        let base = util::sanitize_component(&book.title, "Untitled", 100);
        let mut target = destination.join(format!("{base}.epub"));
        if target.exists() {
            target = destination.join(format!("{base} ({}).epub", book.id));
        }
        if target.exists() {
            bail!("{} already exists", target.display());
        }
        let temporary = destination.join(format!(".clibrary-export-{}.epub", std::process::id()));
        util::remove_file_if_exists(&temporary);
        util::copy_verified(&source, &temporary, &target, &book.hash)?;
        Ok(Export { book, path: target })
    }

    pub fn missing(&self) -> Vec<&Book> {
        self.index
            .books
            .iter()
            .filter(|book| !self.exists(book))
            .collect()
    }
}

fn ambiguous(query: &str, matches: &[&Book]) -> anyhow::Error {
    let mut message = format!(
        "{query:?} matches {} books; use an ID prefix:\n",
        matches.len()
    );
    for book in matches.iter().take(8) {
        message.push_str(&format!(
            "  {}  {} — {}\n",
            book.id,
            util::truncate_chars(&book.title, 60),
            book.authors_display()
        ));
    }
    anyhow::anyhow!("{message}")
}

fn validate_index(index: &Index) -> Result<()> {
    if index.version > INDEX_VERSION {
        bail!(
            "the library index uses format version {} but this build only supports up to {}",
            index.version,
            INDEX_VERSION
        );
    }
    let mut ids = HashSet::new();
    for book in &index.books {
        util::validate_relative_path(&book.epub_path, "books")
            .with_context(|| format!("invalid EPUB path for book {}", book.id))?;
        if let Some(cover) = &book.cover_path {
            util::validate_relative_path(cover, "books")
                .with_context(|| format!("invalid cover path for book {}", book.id))?;
        }
        if book.id.len() < 8 || !book.id.chars().all(|ch| ch.is_ascii_hexdigit()) {
            bail!("invalid book ID {:?} in the library index", book.id);
        }
        if !ids.insert(&book.id) {
            bail!("duplicate book ID {} in the library index", book.id);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{EpubSpec, write_epub};

    fn setup() -> (tempfile::TempDir, Library) {
        let temporary = tempfile::tempdir().unwrap();
        let library = Library::open(temporary.path().join("library")).unwrap();
        (temporary, library)
    }

    fn add_book(library: &mut Library, directory: &Path, spec: &EpubSpec) -> Book {
        let source = directory.join(format!("{}.epub", spec.title.as_deref().unwrap_or("book")));
        write_epub(&source, spec).unwrap();
        match library.add_epub(&source).unwrap() {
            AddOutcome::Added(book) => book,
            AddOutcome::Duplicate(book) => panic!("unexpected duplicate: {}", book.title),
        }
    }

    #[test]
    fn adds_copies_and_deduplicates() {
        let (temporary, mut library) = setup();
        let source = temporary.path().join("incoming.epub");
        write_epub(&source, &EpubSpec::new("The Hobbit", "J.R.R. Tolkien")).unwrap();
        let (source_hash, source_size) = util::hash_file(&source).unwrap();

        let outcome = library.add_epub(&source).unwrap();
        let book = match outcome {
            AddOutcome::Added(book) => book,
            AddOutcome::Duplicate(book) => panic!("unexpected duplicate: {}", book.title),
        };

        assert_eq!(book.title, "The Hobbit");
        assert_eq!(book.authors, vec!["J.R.R. Tolkien"]);
        assert_eq!(book.epub_path, "books/J.R.R. Tolkien/The Hobbit/book.epub");
        assert_eq!(book.size, source_size);
        assert_eq!(book.hash, source_hash);
        assert!(library.book_path(&book).is_file());
        assert!(library.book_dir(&book).join("cover.jpg").is_file());
        assert!(library.index_path().is_file());

        let (copied_hash, _) = util::hash_file(&library.book_path(&book)).unwrap();
        assert_eq!(copied_hash, source_hash);

        let duplicate = library.add_epub(&source).unwrap();
        match duplicate {
            AddOutcome::Duplicate(existing) => assert_eq!(existing.id, book.id),
            AddOutcome::Added(_) => panic!("the same file was added twice"),
        }
        assert_eq!(library.books().len(), 1);

        let reopened = Library::open(library.root().to_path_buf()).unwrap();
        assert_eq!(reopened.books().len(), 1);
        assert_eq!(reopened.books()[0].title, "The Hobbit");
    }

    #[test]
    fn same_title_by_same_author_gets_unique_directory() {
        let (temporary, mut library) = setup();
        let first = EpubSpec {
            title: Some("Collected Works".to_string()),
            author: Some("An Author".to_string()),
            date: Some("1990".to_string()),
            ..EpubSpec::default()
        };
        let second = EpubSpec {
            date: Some("2001".to_string()),
            ..first.clone()
        };
        let book_one = add_book(&mut library, temporary.path(), &first);
        let book_two = add_book(&mut library, temporary.path(), &second);
        assert_ne!(book_one.epub_path, book_two.epub_path);
        assert!(book_two.epub_path.contains(" ("));
        assert!(library.book_path(&book_one).is_file());
        assert!(library.book_path(&book_two).is_file());
    }

    #[test]
    fn removes_index_entry_and_files() {
        let (temporary, mut library) = setup();
        let book = add_book(
            &mut library,
            temporary.path(),
            &EpubSpec::new("Removable", "Someone"),
        );
        let directory = library.book_dir(&book);
        assert!(directory.is_dir());

        let removal = library.remove(&book.id, false).unwrap();
        assert!(removal.files_removed);
        assert!(!directory.exists());
        assert!(library.books().is_empty());
        assert!(library.index_path().is_file());

        let second = add_book(
            &mut library,
            temporary.path(),
            &EpubSpec::new("Keep Me", "Someone"),
        );
        let directory = library.book_dir(&second);
        let removal = library.remove(&second.id, true).unwrap();
        assert!(!removal.files_removed);
        assert!(removal.warning.is_some());
        assert!(directory.is_dir());
        assert!(library.books().is_empty());
    }

    #[test]
    fn resolves_ids_prefixes_and_queries() {
        let (temporary, mut library) = setup();
        let alpha = add_book(
            &mut library,
            temporary.path(),
            &EpubSpec::new("Alpha", "Same Author"),
        );
        let beta = add_book(
            &mut library,
            temporary.path(),
            &EpubSpec::new("Beta", "Same Author"),
        );

        assert_eq!(library.resolve(&alpha.id).unwrap().id, alpha.id);
        assert_eq!(library.resolve(&alpha.id[..8]).unwrap().id, alpha.id);
        assert_eq!(library.resolve("Alpha").unwrap().id, alpha.id);
        assert_eq!(library.resolve("beta").unwrap().id, beta.id);
        assert!(library.resolve("Same Author").is_err());
        assert!(library.resolve("nothing here").is_err());
    }

    #[test]
    fn exports_without_overwriting() {
        let (temporary, mut library) = setup();
        let book = add_book(
            &mut library,
            temporary.path(),
            &EpubSpec::new("Exportable", "An Author"),
        );
        let output = temporary.path().join("exports");
        let first = library.export(&book.id, &output).unwrap();
        assert!(first.path.is_file());
        let (hash, _) = util::hash_file(&first.path).unwrap();
        assert_eq!(hash, book.hash);

        let second = library.export(&book.id, &output).unwrap();
        assert_ne!(first.path, second.path);
        assert!(second.path.is_file());
    }

    #[test]
    fn rejects_files_inside_library_and_non_epubs() {
        let (temporary, mut library) = setup();
        let book = add_book(
            &mut library,
            temporary.path(),
            &EpubSpec::new("Inside", "An Author"),
        );
        let error = library.add_epub(&library.book_path(&book)).unwrap_err();
        assert!(format!("{error:#}").contains("inside the library"));

        let text = temporary.path().join("notes.txt");
        fs::write(&text, b"hello").unwrap();
        assert!(library.add_epub(&text).is_err());
    }

    #[test]
    fn persists_and_clears_reading_progress() {
        let (temporary, mut library) = setup();
        let book = add_book(
            &mut library,
            temporary.path(),
            &EpubSpec::new("Read Me", "An Author"),
        );
        assert!(book.reading_progress.is_none());

        let progress = ReadingProgress {
            chapter: 1,
            chapters: 3,
            fraction: 0.5,
            updated_at: 0,
        };
        library.set_reading_progress(&book.id, progress).unwrap();

        let stored = library.books()[0].reading_progress.unwrap();
        assert_eq!(stored.chapter, 1);
        assert_eq!(stored.chapters, 3);
        assert!((stored.fraction - 0.5).abs() < f64::EPSILON);
        assert!(stored.updated_at > 0);

        let reopened = Library::open(library.root().to_path_buf()).unwrap();
        let reloaded = reopened.books()[0].reading_progress.unwrap();
        assert_eq!(reloaded.chapter, 1);
        assert_eq!(reloaded.percent(), 50);

        library.remove(&book.id, false).unwrap();
        let reopened = Library::open(library.root().to_path_buf()).unwrap();
        assert!(reopened.books().is_empty());
    }

    #[test]
    fn records_the_last_open_time() {
        let (temporary, mut library) = setup();
        let book = add_book(
            &mut library,
            temporary.path(),
            &EpubSpec::new("Open Me", "An Author"),
        );
        assert!(book.last_opened_at.is_none());

        let progress = ReadingProgress {
            chapter: 1,
            chapters: 3,
            fraction: 0.5,
            updated_at: 0,
        };
        library.set_reading_progress(&book.id, progress).unwrap();
        library.record_opened(&book.id).unwrap();

        let stored = library.books()[0].clone();
        assert!(stored.last_opened_at.is_some_and(|opened| opened > 0));
        assert_eq!(
            stored.reading_progress.map(|progress| progress.chapter),
            Some(1)
        );

        let reopened = Library::open(library.root().to_path_buf()).unwrap();
        assert_eq!(reopened.books()[0].last_opened_at, stored.last_opened_at);

        assert!(library.record_opened("missing").is_err());
    }

    #[test]
    fn persists_reader_settings() {
        let (_temporary, mut library) = setup();
        assert_eq!(library.reader_settings().column_width, None);

        library
            .set_reader_settings(ReaderSettings {
                column_width: Some(52),
            })
            .unwrap();
        assert_eq!(library.reader_settings().column_width, Some(52));

        let reopened = Library::open(library.root().to_path_buf()).unwrap();
        assert_eq!(reopened.reader_settings().column_width, Some(52));

        library
            .set_reader_settings(ReaderSettings { column_width: None })
            .unwrap();
        let reopened = Library::open(library.root().to_path_buf()).unwrap();
        assert_eq!(reopened.reader_settings().column_width, None);
    }

    #[test]
    fn refuses_unsafe_paths_in_index() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        fs::create_dir_all(root.join("books")).unwrap();
        fs::write(
            root.join("index.json"),
            r#"{
                "version": 1,
                "books": [{
                    "id": "abcdef123456",
                    "hash": "abcdef1234567890",
                    "title": "Evil",
                    "epub_path": "../outside.epub",
                    "added_at": 0,
                    "size": 1
                }]
            }"#,
        )
        .unwrap();
        assert!(Library::open(&root).is_err());
    }
}
