# clibrary

A keyboard-driven personal library and reader for EPUB files. Add an EPUB once and
`clibrary` reads its metadata, stores a verified copy in a private library, and
lets you browse, search, and sort it from the terminal.

```
 clibrary   /home/you/.local/share/clibrary
 10 books · sort: recent ↓
┌ Books (10)───────────────────────────────────────────────────────────────┐
│  Title                        Author              Series          Year   │
│▶ Dune                         Frank Herbert       Dune Chronicles #1 1965│
│  Dune Messiah                 Frank Herbert       Dune Chronicles #2 1969│
│  The Hobbit                   J.R.R. Tolkien      Middle-earth #1    1937│
│  …                                                                       │
└──────────────────────────────────────────────────────────────────────────┘
┌ Details ─────────────────────────────────────────────────────────────────┐
│ Dune  7b80f652d0e5                                                       │
│ by Frank Herbert                                                         │
│ Dune Chronicles #1 · 1965-08-01 · Chilton · en                           │
│ File: books/Frank Herbert/Dune/book.epub  1.7 MiB · added 2026-10-27     │
└──────────────────────────────────────────────────────────────────────────┘
 q quit · j/k · / search · s sort · Enter read · o open · d delete · ? help
```

## Features

- **EPUB 2 and EPUB 3 metadata** — title, authors (and `file-as` sort names),
  series and series index, publisher, publication date, language, subjects,
  identifiers, description, and cover image.
- **Safe by default** — imports are copied into a temp file, checked against the
  source SHA-256, and only then committed. Duplicate content is skipped.
- **Recent-first sorting** — the default order shows the most recently added,
  opened, or read books first. You can also sort by author, title, series,
  publication date, or the date you added a book. Missing metadata always sorts
  last.
- **Search** across title, authors, series, subjects, description, identifiers,
  and book ID.
- **A real TUI** — browse, filter, inspect, read, open, and delete books without
  leaving the terminal.
- **In-app reading mode** — read EPUB text directly in the terminal, with a
  table of contents and page-by-page navigation. Wide terminals automatically
  use two columns with sensible maximum line lengths. Your place is saved when
  you leave the reader, so reopening a book resumes where you stopped.
- **Scriptable commands** — `list --json`, `show --json`, stable exit codes.
- **Private storage** — the library defaults to
  `$XDG_DATA_HOME/clibrary` (usually `~/.local/share/clibrary`) with private
  directory and file permissions.
- **Verification and export** — check for missing files and copy books back out
  without overwriting anything.

## Install

Requires Rust 1.88 or newer.

```sh
cargo install --path .
```

Or build a release binary:

```sh
cargo build --release
./target/release/clibrary --help
```

## Quick start

```sh
# Import one book, several books, or every EPUB under a directory
clibrary add ~/Downloads/book.epub
clibrary add ~/Downloads/

# Same, but remove the source after each verified copy
clibrary add --move ~/Downloads/book.epub

# Preview an import without changing anything
clibrary add --dry-run ~/Downloads/

# Open the browser
clibrary

# Scriptable use
clibrary list --sort title
clibrary list --json
clibrary show dune
clibrary show 7b80f652d0e5
clibrary export "dune" --to ~/Desktop
```

The library is stored in `$XDG_DATA_HOME/clibrary` by default. Override it with
`--library DIR` or `CLIBRARY_DIR`:

```sh
clibrary --library ~/Books/clibrary add ~/Downloads/*.epub
CLIBRARY_DIR=~/Books/clibrary clibrary
```

## Commands

| Command | Description |
| --- | --- |
| `clibrary` | Open the interactive browser (same as `clibrary tui`). |
| `clibrary add <PATH>...` | Import EPUB files, or scan directories recursively. `--move` deletes sources after a verified copy; `--dry-run` previews; `--json` prints results. |
| `clibrary list [QUERY]` | List books, optionally filtered. `--sort recent\|author\|title\|series\|published\|added` (also `date`/`year` for published), `--reverse`, `--json`. Defaults to `recent`. |
| `clibrary show <QUERY>` | Show full metadata for one book. `--json`. |
| `clibrary open <QUERY>` | Open the book in the system EPUB viewer. |
| `clibrary export <QUERY> --to <DIR>` | Copy a book out of the library (never overwrites). |
| `clibrary remove <QUERY>` | Remove a book. Asks for confirmation; `--yes` is required in scripts. `--keep-files` keeps the files. |
| `clibrary verify` | Report books whose EPUB file has gone missing (exit code 1 if any). |
| `clibrary where` | Print the library location, index path, and book count. |

`<QUERY>` accepts a full 12-character book ID, an ID prefix of at least four
characters, or a case-insensitive metadata search. Ambiguous searches list the
matching IDs so you can pick one.

Add JSON output includes a `status` of `added`, `duplicate`, `would-add`, or
`error` for every input.

## Browser keys

| Key | Action |
| --- | --- |
| `j` / `↓`, `k` / `↑` | Next / previous book |
| `g` / `Home`, `G` / `End` | First / last book |
| `PgDn` / `PgUp` | Move ten books |
| `Tab` | Focus the Details pane (`j`/`k`, `PgDn`/`PgUp`, `g`/`G` scroll; `Tab`/`Esc` back) |
| `/` | Search (live filter; `Enter` applies, `Esc` cancels) |
| `Esc` | Clear the active filter, or quit when there is no filter |
| `s` | Cycle sort: recent → author → title → series → published → added |
| `S` | Reverse the sort order (`recent` starts newest-first) |
| `Enter` | Read the selected EPUB inside `clibrary` |
| `o` | Open the selected EPUB in the system viewer |
| `d` | Delete the book and its files (asks for `y`/`n`) |
| `r` | Reload the index from disk |
| `?` | Toggle the help overlay |
| `q` / `Ctrl-C` | Quit |

Books whose EPUB file has disappeared are shown dimmed with `(missing)`.

The `recent` sort uses the latest of three timestamps: when a book was added,
when it was last opened (`Enter`, `o`, or `clibrary open`), and when its reading
position was last saved. Opening or reading a book therefore moves it back to
the top. Press `S` to see the oldest activity first instead.

## Reading mode

Press `Enter` on a book to read its text in the terminal. Reading mode never
changes the EPUB or any book files; it records your position in the library
index when you leave the reader or quit. Reopening a book resumes at the same
chapter and relative position, even if the terminal size has changed. The
library's Details panel and `clibrary show` display saved progress.

The reader uses a comfortable maximum column width of 76 cells. One column is
used on ordinary terminals; when the window is wide enough, the text is split
into two columns and read left column first, then right. `c` cycles between
automatic, single-column, and two-column layouts. Resizing the terminal reflows
the text while keeping your approximate place in the chapter. `+` and `-` widen
or narrow the columns in four-cell steps (from 20 up to 76 cells); `0` restores
the automatic width. Your chosen width is remembered in the library index and
reused for every book, including after you restart `clibrary`.

Book text always moves by whole pages. There is no line-by-line scrolling, so a
page turn replaces the whole screen (or spread) and dual-column pages never
leave a stray one-line offset. Turning past the end of a chapter continues into
the next chapter; turning back at the start of a chapter returns to the end of
the previous one.

| Key | Action |
| --- | --- |
| `j` / `l` / `↓` / `→` / `Space` / `PgDn` | Next page |
| `k` / `h` / `↑` / `←` / `PgUp` | Previous page |
| `g` / `Home` | Start of chapter |
| `G` / `End` | End of chapter |
| `]` / `[` | Next / previous chapter |
| `t` / `Tab` | Table of contents |
| `m` | Book metadata |
| `c` | Cycle columns: auto → one → two |
| `+` / `-` | Wider / narrower columns |
| `0` | Reset column width |
| `?` | Reading-key help |
| `q` / `Esc` | Back to the library |

The table of contents uses EPUB 3 navigation documents when available and
falls back to EPUB 2 NCX, then to chapter headings. Headings, paragraphs,
blockquotes, lists, preformatted text, and inline emphasis are preserved; images
are skipped. EPUBs whose content is encrypted (DRM) cannot be read this way.

The `m` metadata popup and the browser's Details pane scroll with `j`/`k`,
`PgDn`/`PgUp`, and `g`/`G`. Close the metadata popup with `m`, `Esc`, `q`, or
`Enter`.

## Library layout

```
library/
├── index.json          # metadata, progress, and reader settings (atomic rewrite)
├── library.lock        # advisory lock for writers
└── books/
    └── Author Name/
        └── Book Title/
            ├── book.epub
            └── cover.jpg
```

If a second book has the same title and author, its directory gets a short
content hash suffix instead of overwriting the first. Covers are stored next to
the EPUB using their original image format.

The index is JSON, one version field, a reader-settings object, and a book
array, so it is easy to inspect or back up. Files are never hard-linked or
modified in place.

## Safety model

- **Original files are read-only** unless you explicitly pass `--move`.
- **Verified copies**: every import is written to a temporary file, hashed, and
  compared with the source SHA-256 before it is renamed into place.
- **Content deduplication**: the SHA-256 hash is the book's identity, so adding
  the same file twice is a no-op even if it was renamed.
- **Atomic index updates**: the index is written to a temp file, fsynced, and
  renamed; a lock serializes concurrent imports and removals.
- **Private permissions**: on Unix, the library and book directories are `0700`
  and books, covers, and the index are `0600`.
- **No archive extraction**: metadata, covers, and reading-mode text are read
  from the EPUB in memory. Paths from EPUB manifests are resolved without
  allowing `..` to escape, and only fixed safe filenames are written. Reading
  mode refuses encrypted content rather than attempting to remove DRM.
- **Guarded deletion**: paths loaded from the index must be relative, start with
  `books/`, and contain no `.`/`..` components. Deletion refuses anything
  outside the library's `books/` directory, and a `books/` symlink that points
  outside the library is rejected at startup.
- **No overwrites**: imports choose a new directory, exports choose a new
  filename, and removal asks for confirmation.
- **DRM is not removed.** Encrypted EPUBs are copied and indexed like any other
  file, but opening them still requires the appropriate reader.

## Metadata notes

- EPUB 2 `calibre:series` / `calibre:series_index` and EPUB 3
  `belongs-to-collection` / `group-position` are supported.
- Author `file-as` metadata is used for author sorting when present.
- `urn:isbn:` identifiers are normalized to an `ISBN` scheme.
- Descriptions with embedded HTML are converted to plain text.
- Malformed EPUBs or files without a package document are rejected with an
  error instead of being imported.

## Development

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

Tests generate EPUB 2/3 fixtures, exercise metadata and chapter extraction, TOC
parsing, wrapping, column layout and pagination, sorting, copy verification,
deduplication, path-traversal rejection, permission handling, the CLI workflow,
and render both the browser and the reader against a test backend.

## License

MIT
