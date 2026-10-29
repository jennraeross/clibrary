#[path = "../src/fixtures.rs"]
mod fixtures;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use clibrary::model::Book;
use fixtures::{EpubSpec, write_epub, write_epub_with_rootfile};
use serde::Deserialize;
use tempfile::TempDir;

struct TestEnv {
    temporary: TempDir,
    library: PathBuf,
}

impl TestEnv {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let library = temporary.path().join("library");
        Self { temporary, library }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_clibrary"));
        command.arg("--library").arg(&self.library);
        command
    }

    fn run(&self, arguments: &[&str]) -> Output {
        self.command().args(arguments).output().unwrap()
    }

    fn add(&self, path: &Path) -> Output {
        self.command().arg("add").arg(path).output().unwrap()
    }

    fn add_spec(&self, spec: &EpubSpec) -> (PathBuf, Output) {
        let path = self
            .temporary
            .path()
            .join(format!("{}.epub", spec.title.as_deref().unwrap_or("book")));
        write_epub(&path, spec).unwrap();
        let output = self.add(&path);
        (path, output)
    }

    fn index(&self) -> Vec<Book> {
        #[derive(Deserialize)]
        struct Index {
            books: Vec<Book>,
        }
        let path = self.library.join("index.json");
        if !path.exists() {
            return Vec::new();
        }
        let bytes = fs::read(path).unwrap();
        serde_json::from_slice::<Index>(&bytes).unwrap().books
    }

    fn book_dir(&self, book: &Book) -> PathBuf {
        self.library
            .join(&book.epub_path)
            .parent()
            .unwrap()
            .to_path_buf()
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        stdout(output),
        stderr(output)
    );
}

#[test]
fn full_workflow_add_list_show_export_remove() {
    let env = TestEnv::new();
    let (hobbit_source, output) = env.add_spec(&EpubSpec {
        title: Some("The Hobbit".to_string()),
        author: Some("J.R.R. Tolkien".to_string()),
        date: Some("1937-09-21".to_string()),
        ..EpubSpec::default()
    });
    assert_success(&output);
    assert!(stdout(&output).contains("added"));
    assert!(stdout(&output).contains("The Hobbit"));
    assert!(stdout(&output).contains("J.R.R. Tolkien"));

    let books = env.index();
    assert_eq!(books.len(), 1);
    let hobbit = &books[0];
    assert_eq!(hobbit.title, "The Hobbit");
    assert_eq!(
        hobbit.epub_path,
        "books/J.R.R. Tolkien/The Hobbit/book.epub"
    );
    assert!(env.library.join(&hobbit.epub_path).is_file());
    assert!(env.book_dir(hobbit).join("cover.jpg").is_file());

    let duplicate = env.add(&hobbit_source);
    assert_success(&duplicate);
    assert!(stdout(&duplicate).contains("skipped"));
    assert_eq!(env.index().len(), 1);

    let (_dune_source, output) = env.add_spec(&EpubSpec {
        title: Some("Dune".to_string()),
        author: Some("Frank Herbert".to_string()),
        date: Some("1965-08-01".to_string()),
        ..EpubSpec::default()
    });
    assert_success(&output);
    assert_eq!(env.index().len(), 2);

    let list = env.run(&["list", "--json"]);
    assert_success(&list);
    let listed: Vec<Book> = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].title, "Dune");
    assert_eq!(listed[1].title, "The Hobbit");

    let reversed = env.run(&["list", "--sort", "title", "--reverse"]);
    assert_success(&reversed);
    let reversed_text = stdout(&reversed);
    let lines: Vec<&str> = reversed_text.lines().collect();
    assert!(lines[0].contains("The Hobbit"));
    assert!(lines[1].contains("Dune"));

    let filtered = env.run(&["list", "hobbit"]);
    assert_success(&filtered);
    assert!(stdout(&filtered).contains("The Hobbit"));
    assert!(!stdout(&filtered).contains("Dune"));

    let show = env.run(&["show", &hobbit.id[..8], "--json"]);
    assert_success(&show);
    let shown: Book = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(shown.id, hobbit.id);
    assert_eq!(shown.published.as_deref(), Some("1937-09-21"));
    assert_eq!(shown.identifiers[0].scheme.as_deref(), Some("ISBN"));
    assert_eq!(shown.description.as_deref(), Some("A test book & sample."));

    let export_dir = env.temporary.path().join("exports");
    let export = env.run(&["export", &hobbit.id, "--to", export_dir.to_str().unwrap()]);
    assert_success(&export);
    let exported = export_dir.join("The Hobbit.epub");
    assert!(exported.is_file());
    assert_eq!(
        fs::read(&exported).unwrap(),
        fs::read(env.library.join(&hobbit.epub_path)).unwrap()
    );

    let directory = env.book_dir(hobbit);
    let remove = env.run(&["remove", &hobbit.id, "--yes"]);
    assert_success(&remove);
    assert!(stdout(&remove).contains("removed"));
    assert_eq!(env.index().len(), 1);
    assert!(!directory.exists());

    let verify = env.run(&["verify"]);
    assert_success(&verify);
    assert!(stdout(&verify).contains("all 1 book files are present"));

    let where_is = env.run(&["where"]);
    assert_success(&where_is);
    assert!(stdout(&where_is).contains("Books:    1"));
}

#[test]
fn scans_directories_dry_runs_and_moves() {
    let env = TestEnv::new();
    let incoming = env.temporary.path().join("incoming");
    fs::create_dir_all(incoming.join("nested")).unwrap();
    write_epub(
        &incoming.join("one.epub"),
        &EpubSpec::new("Book One", "Author One"),
    )
    .unwrap();
    write_epub(
        &incoming.join("nested/two.epub"),
        &EpubSpec::new("Book Two", "Author Two"),
    )
    .unwrap();
    fs::write(incoming.join("notes.txt"), b"ignore me").unwrap();

    let output = env.add(&incoming);
    assert_success(&output);
    assert_eq!(env.index().len(), 2);
    assert_eq!(stdout(&output).matches("added").count(), 2);

    let dry_source = env.temporary.path().join("dry.epub");
    write_epub(&dry_source, &EpubSpec::new("Dry Run", "Nobody")).unwrap();
    let dry = env
        .command()
        .arg("add")
        .arg("--dry-run")
        .arg(&dry_source)
        .output()
        .unwrap();
    assert_success(&dry);
    assert!(stdout(&dry).contains("would add"));
    assert_eq!(env.index().len(), 2);
    assert!(!env.library.join("books/Nobody/Dry Run/book.epub").exists());

    let existing = env.add(&incoming.join("one.epub"));
    assert_success(&existing);
    assert!(stdout(&existing).contains("skipped"));

    let missing_path = env.add(&env.temporary.path().join("nope.epub"));
    assert!(!missing_path.status.success());
    assert!(stdout(&missing_path).contains("does not exist"));

    let empty = env.temporary.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let empty_output = env.add(&empty);
    assert!(!empty_output.status.success());
    assert!(stdout(&empty_output).contains("no EPUB files found"));

    // Verify reports a file that was removed behind the library's back.
    let books = env.index();
    let removed_file_dir = env.book_dir(&books[0]);
    fs::remove_file(env.library.join(&books[0].epub_path)).unwrap();
    let verify = env.run(&["verify"]);
    assert!(!verify.status.success());
    assert!(stdout(&verify).contains("missing"));
    let where_is = env.run(&["where"]);
    assert!(stdout(&where_is).contains("1 missing"));

    // keep_files removes the entry but leaves the directory alone.
    let keep = env.run(&["remove", &books[1].id, "--yes", "--keep-files"]);
    assert_success(&keep);
    assert!(stdout(&keep).contains("files were left"));
    assert!(env.book_dir(&books[1]).is_dir());

    // --move removes the incoming file only after the copy is committed.
    let movable = env.temporary.path().join("movable.epub");
    write_epub(&movable, &EpubSpec::new("Movable", "Mover")).unwrap();
    let output = env
        .command()
        .arg("add")
        .arg("--move")
        .arg(&movable)
        .output()
        .unwrap();
    assert_success(&output);
    assert!(!movable.exists());
    assert!(env.library.join("books/Mover/Movable/book.epub").is_file());

    // The keep-files directory is untouched, but nothing points at it anymore.
    assert!(removed_file_dir.is_dir());
}

#[test]
fn rejects_invalid_and_unsafe_inputs() {
    let env = TestEnv::new();
    let fake = env.temporary.path().join("fake.epub");
    fs::write(&fake, b"this is not a zip archive").unwrap();
    let output = env.add(&fake);
    assert!(!output.status.success());
    assert!(stdout(&output).contains("error"));
    assert!(stdout(&output).contains("not a valid ZIP"));
    assert_eq!(env.index().len(), 0);

    let text = env.temporary.path().join("book.txt");
    fs::write(&text, b"plain text").unwrap();
    let output = env.add(&text);
    assert!(!output.status.success());
    assert!(stdout(&output).contains("not an EPUB file"));

    let traversal = env.temporary.path().join("traversal.epub");
    write_epub_with_rootfile(&traversal, "../outside.opf").unwrap();
    let output = env.add(&traversal);
    assert!(!output.status.success());
    assert!(stdout(&output).contains("error"));
    assert!(!env.temporary.path().join("outside.opf").exists());
    assert!(!env.temporary.path().join("library/outside.opf").exists());
    assert_eq!(env.index().len(), 0);

    // Removing without --yes is refused when there is no terminal to prompt.
    let (_source, add) = env.add_spec(&EpubSpec::new("Safe", "Author"));
    assert_success(&add);
    let id = env.index()[0].id.clone();
    let output = env.run(&["remove", &id]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("pass --yes"));
    assert_eq!(env.index().len(), 1);
}

#[test]
fn refuses_to_import_files_already_inside_the_library() {
    let env = TestEnv::new();
    let (_source, output) = env.add_spec(&EpubSpec::new("Inside", "Author"));
    assert_success(&output);
    let book = env.index().remove(0);
    let path = env.library.join(&book.epub_path);
    let output = env.add(&path);
    assert!(!output.status.success());
    assert!(stdout(&output).contains("inside the library"));
    assert_eq!(env.index().len(), 1);
}

#[test]
fn honours_clibrary_dir_environment_variable() {
    let temporary = tempfile::tempdir().unwrap();
    let library = temporary.path().join("env-library");
    let source = temporary.path().join("env.epub");
    write_epub(&source, &EpubSpec::new("Env Book", "Env Author")).unwrap();

    let add = Command::new(env!("CARGO_BIN_EXE_clibrary"))
        .env("CLIBRARY_DIR", &library)
        .arg("add")
        .arg(&source)
        .output()
        .unwrap();
    assert_success(&add);
    assert!(library.join("index.json").is_file());

    let list = Command::new(env!("CARGO_BIN_EXE_clibrary"))
        .env("CLIBRARY_DIR", &library)
        .arg("list")
        .output()
        .unwrap();
    assert_success(&list);
    assert!(stdout(&list).contains("Env Book"));
}

#[test]
fn interactive_mode_requires_a_terminal() {
    let env = TestEnv::new();
    let output = env.run(&[]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("needs a terminal"));
}

#[cfg(unix)]
#[test]
fn library_files_are_private() {
    use std::os::unix::fs::PermissionsExt;

    let env = TestEnv::new();
    let (_source, output) = env.add_spec(&EpubSpec::new("Private", "Author"));
    assert_success(&output);
    let book = env.index().remove(0);

    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&env.library), 0o700);
    assert_eq!(mode(&env.library.join("books")), 0o700);
    assert_eq!(mode(&env.library.join(&book.epub_path)), 0o600);
    assert_eq!(mode(&env.library.join("index.json")), 0o600);
    assert_eq!(mode(&env.book_dir(&book).join("cover.jpg")), 0o600);
}
