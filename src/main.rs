use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::Serialize;
use walkdir::WalkDir;

use clibrary::library::{AddOutcome, Library};
use clibrary::model::{Book, SortKey, sorted_indices};
use clibrary::{epub, tui, util};

#[derive(Parser)]
#[command(
    name = "clibrary",
    version,
    about = "A navigable personal library for EPUB files",
    long_about = "Import EPUBs into a private library, keep their metadata in a searchable index, \
                  and browse them in a keyboard-driven terminal UI.",
    after_help = "Examples:\n  clibrary add ~/Downloads/book.epub\n  clibrary\n  clibrary list --sort published --reverse\n  clibrary show 3f2a1b\n  clibrary export \"dune\" --to ~/Desktop"
)]
struct Cli {
    /// Library directory [default: $XDG_DATA_HOME/clibrary]
    #[arg(long, global = true, env = "CLIBRARY_DIR", value_name = "DIR")]
    library: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Open the interactive browser (the default when no command is given)
    Tui {
        /// Metadata field to sort by
        #[arg(long, value_enum, default_value_t = SortKey::Recent)]
        sort: SortKey,
        /// Reverse the sort order
        #[arg(long)]
        reverse: bool,
    },
    /// Import EPUB files or directories into the library
    Add {
        /// EPUB files or directories to scan for EPUBs
        #[arg(required = true, value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// Remove source files after the verified copy is committed
        #[arg(long = "move")]
        move_files: bool,
        /// Show what would be imported without changing anything
        #[arg(long)]
        dry_run: bool,
        /// Print machine-readable results
        #[arg(long)]
        json: bool,
    },
    /// List books, optionally filtered by a search query
    List {
        /// Filter by title, author, series, subject, description or ID
        #[arg(value_name = "QUERY")]
        query: Option<String>,
        /// Metadata field to sort by
        #[arg(long, value_enum, default_value_t = SortKey::Recent)]
        sort: SortKey,
        /// Reverse the sort order
        #[arg(long)]
        reverse: bool,
        /// Print the matching books as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show the full metadata of a single book
    Show {
        /// Book ID, ID prefix, or search term
        query: String,
        /// Print the book as JSON
        #[arg(long)]
        json: bool,
    },
    /// Remove a book from the index and delete its files
    Remove {
        /// Book ID, ID prefix, or search term
        query: String,
        /// Do not ask for confirmation
        #[arg(long, short = 'y')]
        yes: bool,
        /// Remove the index entry but leave the files in place
        #[arg(long)]
        keep_files: bool,
    },
    /// Open a book in the system EPUB viewer
    Open {
        /// Book ID, ID prefix, or search term
        query: String,
    },
    /// Copy a book out of the library
    Export {
        /// Book ID, ID prefix, or search term
        query: String,
        /// Destination directory
        #[arg(long, short = 'o', value_name = "DIR")]
        to: PathBuf,
    },
    /// Print the library location and book count
    Where,
    /// Report books whose EPUB files are missing
    Verify,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    let root = resolve_library_path(cli.library)?;

    let Some(command) = cli.command else {
        let library = Library::open(root)?;
        tui::run(library, SortKey::Recent, false)?;
        return Ok(ExitCode::SUCCESS);
    };

    match command {
        Command::Tui { sort, reverse } => {
            let library = Library::open(root)?;
            tui::run(library, sort, reverse)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Add {
            paths,
            move_files,
            dry_run,
            json,
        } => add(&root, &paths, move_files, dry_run, json),
        Command::List {
            query,
            sort,
            reverse,
            json,
        } => list(&root, query.as_deref(), sort, reverse, json),
        Command::Show { query, json } => show(&root, &query, json),
        Command::Remove {
            query,
            yes,
            keep_files,
        } => remove(&root, &query, yes, keep_files),
        Command::Open { query } => open(&root, &query),
        Command::Export { query, to } => export(&root, &query, &to),
        Command::Where => where_is(&root),
        Command::Verify => verify(&root),
    }
}

fn resolve_library_path(argument: Option<PathBuf>) -> Result<PathBuf> {
    match argument {
        Some(path) => Ok(expand_tilde(path)),
        None => {
            let base = dirs::data_dir().context(
                "cannot determine the user data directory; pass --library or set CLIBRARY_DIR",
            )?;
            Ok(base.join("clibrary"))
        }
    }
}

fn expand_tilde(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if text == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    } else if let Some(rest) = text.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    path
}

#[derive(Serialize)]
struct AddReport {
    source: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    authors: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl AddReport {
    fn error(source: &Path, error: impl std::fmt::Display) -> Self {
        Self {
            source: source.display().to_string(),
            status: "error",
            id: None,
            title: None,
            authors: None,
            path: None,
            warning: None,
            error: Some(error.to_string()),
        }
    }

    fn added(book: &Book) -> Self {
        Self {
            source: String::new(),
            status: "added",
            id: Some(book.id.clone()),
            title: Some(book.title.clone()),
            authors: Some(book.authors_display()),
            path: Some(book.epub_path.clone()),
            warning: None,
            error: None,
        }
    }

    fn duplicate(book: &Book) -> Self {
        Self {
            source: String::new(),
            status: "duplicate",
            id: Some(book.id.clone()),
            title: Some(book.title.clone()),
            authors: Some(book.authors_display()),
            path: Some(book.epub_path.clone()),
            warning: None,
            error: None,
        }
    }

    fn would_add(title: String, authors: String) -> Self {
        Self {
            source: String::new(),
            status: "would-add",
            id: None,
            title: Some(title),
            authors: Some(authors),
            path: None,
            warning: None,
            error: None,
        }
    }
}

fn add(
    root: &Path,
    paths: &[PathBuf],
    move_files: bool,
    dry_run: bool,
    json: bool,
) -> Result<ExitCode> {
    let mut library = Library::open(root)?;
    let mut reports = Vec::new();
    let mut sources = Vec::new();

    for path in paths {
        if path.is_dir() {
            let mut found = 0usize;
            for entry in WalkDir::new(path).follow_links(false) {
                match entry {
                    Ok(entry) => {
                        if entry.file_type().is_file() && util::is_epub_file(entry.path()) {
                            sources.push(entry.into_path());
                            found += 1;
                        }
                    }
                    Err(error) => reports.push(AddReport::error(
                        error.path().unwrap_or(path),
                        format!("cannot read directory entry: {error}"),
                    )),
                }
            }
            if found == 0 {
                reports.push(AddReport::error(path, "no EPUB files found"));
            }
        } else if path.exists() {
            sources.push(path.clone());
        } else {
            reports.push(AddReport::error(path, "path does not exist"));
        }
    }

    sources.sort();
    sources.dedup();

    for source in sources {
        if dry_run {
            match dry_run_add(&library, &source) {
                Ok(mut report) => {
                    report.source = source.display().to_string();
                    reports.push(report);
                }
                Err(error) => reports.push(AddReport::error(&source, format!("{error:#}"))),
            }
            continue;
        }

        match library.add_epub(&source) {
            Ok(AddOutcome::Added(book)) => {
                let mut report = AddReport::added(&book);
                report.source = source.display().to_string();
                if move_files {
                    match fs::canonicalize(&source) {
                        Ok(absolute) if !absolute.starts_with(library.books_dir()) => {
                            if let Err(error) = fs::remove_file(&absolute) {
                                report.warning = Some(format!(
                                    "verified copy saved, but could not remove source: {error}"
                                ));
                            }
                        }
                        Ok(_) => {
                            report.warning = Some(
                                "source is inside the library; it was not removed".to_string(),
                            );
                        }
                        Err(error) => {
                            report.warning =
                                Some(format!("could not resolve source for removal: {error}"));
                        }
                    }
                }
                reports.push(report);
            }
            Ok(AddOutcome::Duplicate(book)) => {
                let mut report = AddReport::duplicate(&book);
                report.source = source.display().to_string();
                reports.push(report);
            }
            Err(error) => reports.push(AddReport::error(&source, format!("{error:#}"))),
        }
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
    } else {
        print_add_reports(&reports);
    }

    let failed = reports.iter().any(|report| report.status == "error");
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn dry_run_add(library: &Library, source: &Path) -> Result<AddReport> {
    if !util::is_epub_file(source) {
        bail!("not an EPUB file");
    }
    let canonical =
        fs::canonicalize(source).with_context(|| format!("cannot resolve {}", source.display()))?;
    if canonical.starts_with(library.books_dir()) {
        bail!("refusing to add a file that is already inside the library");
    }
    let (hash, _) = util::hash_file(&canonical)?;
    if let Some(existing) = library.find_by_hash(&hash) {
        return Ok(AddReport::duplicate(existing));
    }
    let metadata = epub::read_metadata(source)?;
    let title = metadata.title.unwrap_or_else(|| {
        source
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("Untitled")
            .to_string()
    });
    let authors = if metadata.authors.is_empty() {
        "Unknown author".to_string()
    } else {
        metadata.authors.join(", ")
    };
    Ok(AddReport::would_add(title, authors))
}

fn print_add_reports(reports: &[AddReport]) {
    for report in reports {
        match report.status {
            "added" => {
                println!(
                    "added     {} — {}",
                    report.title.as_deref().unwrap_or("?"),
                    report.authors.as_deref().unwrap_or("?")
                );
                if let Some(path) = &report.path {
                    println!("          {path}");
                }
                if let Some(warning) = &report.warning {
                    println!("          warning: {warning}");
                }
            }
            "duplicate" => {
                println!(
                    "skipped   {} — {}  (already in library: {})",
                    report.title.as_deref().unwrap_or("?"),
                    report.authors.as_deref().unwrap_or("?"),
                    report.id.as_deref().unwrap_or("?")
                );
            }
            "would-add" => {
                println!(
                    "would add {} — {}",
                    report.title.as_deref().unwrap_or("?"),
                    report.authors.as_deref().unwrap_or("?")
                );
            }
            _ => {
                println!(
                    "error     {}: {}",
                    report.source,
                    report.error.as_deref().unwrap_or("unknown error")
                );
            }
        }
    }
}

fn list(
    root: &Path,
    query: Option<&str>,
    sort: SortKey,
    reverse: bool,
    json: bool,
) -> Result<ExitCode> {
    let library = Library::open(root)?;
    let indices = sorted_indices(library.books(), sort, reverse, query);

    if json {
        let books: Vec<&Book> = indices
            .iter()
            .map(|&index| &library.books()[index])
            .collect();
        println!("{}", serde_json::to_string_pretty(&books)?);
        return Ok(ExitCode::SUCCESS);
    }

    if indices.is_empty() {
        if library.books().is_empty() {
            println!("The library is empty. Import books with: clibrary add <file.epub>");
        } else {
            println!("No books match that query.");
        }
        return Ok(ExitCode::SUCCESS);
    }

    for index in indices {
        let book = &library.books()[index];
        let mut line = format!("{}  {} — {}", book.id, book.title, book.authors_display());
        if let Some(year) = book.year() {
            line.push_str(&format!(" ({year})"));
        }
        if let Some(series) = book.series_display() {
            line.push_str(&format!(" · {series}"));
        }
        if !library.exists(book) {
            line.push_str("  [missing]");
        }
        println!("{line}");
    }
    Ok(ExitCode::SUCCESS)
}

fn show(root: &Path, query: &str, json: bool) -> Result<ExitCode> {
    let library = Library::open(root)?;
    let book = library.resolve(query)?;
    if json {
        println!("{}", serde_json::to_string_pretty(book)?);
        return Ok(ExitCode::SUCCESS);
    }

    println!("Title:       {}", book.title);
    println!("Authors:     {}", book.authors_display());
    if let Some(series) = book.series_display() {
        println!("Series:      {series}");
    }
    if let Some(published) = &book.published {
        println!("Published:   {published}");
    }
    if let Some(publisher) = &book.publisher {
        println!("Publisher:   {publisher}");
    }
    if let Some(language) = &book.language {
        println!("Language:    {language}");
    }
    if let Some(progress) = &book.reading_progress {
        println!(
            "Progress:    {}% (chapter {} of {}, updated {})",
            progress.percent(),
            progress.chapter.saturating_add(1),
            progress.chapters.max(1),
            util::format_timestamp(progress.updated_at)
        );
    }
    if !book.subjects.is_empty() {
        println!("Subjects:    {}", book.subjects.join(", "));
    }
    if !book.identifiers.is_empty() {
        let identifiers: Vec<String> = book
            .identifiers
            .iter()
            .map(|identifier| match &identifier.scheme {
                Some(scheme) => format!("{scheme} {}", identifier.value),
                None => identifier.value.clone(),
            })
            .collect();
        println!("Identifiers: {}", identifiers.join(", "));
    }
    println!("Added:       {}", book.added_display());
    println!("Size:        {}", book.size_display());
    println!("ID:          {}", book.id);
    let path = library.book_path(book);
    println!("File:        {}", path.display());
    if !path.is_file() {
        println!("             [file is missing]");
    }
    if let Some(cover) = &book.cover_path {
        println!(
            "Cover:       {}",
            util::library_path(library.root(), cover).display()
        );
    }
    if let Some(description) = &book.description {
        println!("Description:");
        println!("  {description}");
    }
    Ok(ExitCode::SUCCESS)
}

fn remove(root: &Path, query: &str, yes: bool, keep_files: bool) -> Result<ExitCode> {
    let mut library = Library::open(root)?;
    let book = library.resolve(query)?.clone();

    if !yes
        && !ask_confirmation(&format!(
            "Remove \"{}\" — {}? [y/N] ",
            book.title,
            book.authors_display()
        ))?
    {
        println!("Cancelled.");
        return Ok(ExitCode::SUCCESS);
    }

    let removal = library.remove(&book.id, keep_files)?;
    println!(
        "removed   {} — {}",
        removal.book.title,
        removal.book.authors_display()
    );
    if removal.files_removed {
        println!(
            "          deleted {}",
            library.book_dir(&removal.book).display()
        );
    }
    if let Some(warning) = removal.warning {
        println!("          note: {warning}");
    }
    Ok(ExitCode::SUCCESS)
}

fn ask_confirmation(prompt: &str) -> Result<bool> {
    if !io::stdin().is_terminal() {
        bail!("refusing to remove a book without confirmation; pass --yes");
    }
    print!("{prompt}");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn open(root: &Path, query: &str) -> Result<ExitCode> {
    let mut library = Library::open(root)?;
    let book = library.resolve(query)?.clone();
    let path = library.book_path(&book);
    if !path.is_file() {
        bail!("the EPUB file is missing: {}", path.display());
    }
    util::open_in_viewer(&path)?;
    if let Err(error) = library.record_opened(&book.id) {
        eprintln!("warning: {error:#}");
    }
    println!("Opening {}", path.display());
    Ok(ExitCode::SUCCESS)
}

fn export(root: &Path, query: &str, to: &Path) -> Result<ExitCode> {
    let library = Library::open(root)?;
    let exported = library.export(query, &expand_tilde(to.to_path_buf()))?;
    println!(
        "exported  {} — {}",
        exported.book.title,
        exported.book.authors_display()
    );
    println!("          {}", exported.path.display());
    Ok(ExitCode::SUCCESS)
}

fn where_is(root: &Path) -> Result<ExitCode> {
    let library = Library::open(root)?;
    let missing = library.missing().len();
    println!("Library:  {}", library.root().display());
    println!("Index:    {}", library.index_path().display());
    if missing == 0 {
        println!("Books:    {}", library.books().len());
    } else {
        println!("Books:    {} ({} missing)", library.books().len(), missing);
    }
    Ok(ExitCode::SUCCESS)
}

fn verify(root: &Path) -> Result<ExitCode> {
    let library = Library::open(root)?;
    let missing = library.missing();
    if missing.is_empty() {
        println!("ok — all {} book files are present", library.books().len());
        return Ok(ExitCode::SUCCESS);
    }

    println!(
        "{} of {} book files are missing:",
        missing.len(),
        library.books().len()
    );
    for book in missing {
        println!(
            "  {}  {} — {}",
            book.id,
            book.title,
            library.book_path(book).display()
        );
    }
    Ok(ExitCode::FAILURE)
}
