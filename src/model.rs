use std::cmp::Ordering;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

pub const INDEX_VERSION: u32 = 1;

/// How books are ordered in the browser and in `clibrary list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SortKey {
    /// Most recent activity first: added, opened, or read.
    Recent,
    Title,
    Author,
    Series,
    #[value(alias = "date", alias = "year")]
    Published,
    Added,
}

impl SortKey {
    pub const ALL: [SortKey; 6] = [
        SortKey::Recent,
        SortKey::Author,
        SortKey::Title,
        SortKey::Series,
        SortKey::Published,
        SortKey::Added,
    ];

    pub fn next(self) -> Self {
        let idx = Self::ALL.iter().position(|k| *k == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }

    pub fn label(self) -> &'static str {
        match self {
            SortKey::Recent => "recent",
            SortKey::Title => "title",
            SortKey::Author => "author",
            SortKey::Series => "series",
            SortKey::Published => "published",
            SortKey::Added => "added",
        }
    }

    /// Whether this key and direction put the largest/newest value first.
    pub fn descending(self, reverse: bool) -> bool {
        match self {
            // `recent` naturally reads newest first.
            SortKey::Recent => !reverse,
            _ => reverse,
        }
    }

    pub fn arrow(self, reverse: bool) -> &'static str {
        if self.descending(reverse) {
            "↓"
        } else {
            "↑"
        }
    }

    pub fn direction_label(self, reverse: bool) -> &'static str {
        if self.descending(reverse) {
            "descending"
        } else {
            "ascending"
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Index {
    pub version: u32,
    #[serde(default)]
    pub books: Vec<Book>,
    #[serde(default)]
    pub reader: ReaderSettings,
}

impl Default for Index {
    fn default() -> Self {
        Self {
            version: INDEX_VERSION,
            books: Vec::new(),
            reader: ReaderSettings::default(),
        }
    }
}

/// Reader preferences shared by every book in the library.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReaderSettings {
    /// User-requested column width; `None` means the automatic width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column_width: Option<u16>,
}

/// A saved place in a book, stored in the library index.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ReadingProgress {
    /// Zero-based index of the current chapter.
    pub chapter: usize,
    /// Number of readable chapters when the position was saved.
    pub chapters: usize,
    /// Position within the chapter: 0.0 is the start, 1.0 is the end.
    pub fraction: f64,
    /// Unix timestamp (seconds) of the last update.
    #[serde(default)]
    pub updated_at: i64,
}

impl ReadingProgress {
    /// Progress through the whole book, from 0.0 to 1.0.
    pub fn overall_fraction(&self) -> f64 {
        if self.chapters == 0 {
            return 0.0;
        }
        let chapters = self.chapters as f64;
        let chapter = self.chapter.min(self.chapters - 1) as f64;
        let fraction = self.fraction.clamp(0.0, 1.0);
        ((chapter + fraction) / chapters).clamp(0.0, 1.0)
    }

    /// Whole-book progress as a rounded percentage.
    pub fn percent(&self) -> u8 {
        (self.overall_fraction() * 100.0).round().clamp(0.0, 100.0) as u8
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identifier {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheme: Option<String>,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Book {
    /// First 12 hex characters of the SHA-256 of the EPUB contents.
    pub id: String,
    pub hash: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_sort: Option<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series_index: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subjects: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identifiers: Vec<Identifier>,
    /// Path relative to the library root, using `/` separators.
    pub epub_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_path: Option<String>,
    /// Unix timestamp (seconds) of when the book was imported.
    pub added_at: i64,
    /// Unix timestamp (seconds) of when the book was last opened in a viewer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_opened_at: Option<i64>,
    /// Size of the EPUB in bytes.
    pub size: u64,
    /// Where the reader left off, if the book has been read in clibrary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reading_progress: Option<ReadingProgress>,
}

impl Book {
    pub fn authors_display(&self) -> String {
        if self.authors.is_empty() {
            "Unknown author".to_string()
        } else {
            self.authors.join(", ")
        }
    }

    pub fn year(&self) -> Option<i32> {
        self.published.as_deref().and_then(crate::util::parse_year)
    }

    pub fn series_display(&self) -> Option<String> {
        let series = self.series.as_deref()?;
        Some(match self.series_index {
            Some(index) => format!("{series} #{index}"),
            None => series.to_string(),
        })
    }

    pub fn added_display(&self) -> String {
        crate::util::format_timestamp(self.added_at)
    }

    /// Most recent activity: added, opened, or read.
    fn recent_timestamp(&self) -> i64 {
        let read = self
            .reading_progress
            .map(|progress| progress.updated_at)
            .unwrap_or(0);
        self.added_at
            .max(self.last_opened_at.unwrap_or(0))
            .max(read)
    }

    pub fn size_display(&self) -> String {
        crate::util::format_size(self.size)
    }

    pub fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return true;
        }
        let mut haystack = String::new();
        haystack.push_str(&self.title);
        haystack.push('\n');
        haystack.push_str(&self.authors.join(" "));
        haystack.push('\n');
        if let Some(series) = &self.series {
            haystack.push_str(series);
            haystack.push('\n');
        }
        for value in [
            self.publisher.as_deref(),
            self.published.as_deref(),
            self.language.as_deref(),
            self.description.as_deref(),
            Some(self.id.as_str()),
        ]
        .into_iter()
        .flatten()
        {
            haystack.push_str(value);
            haystack.push('\n');
        }
        for subject in &self.subjects {
            haystack.push_str(subject);
            haystack.push('\n');
        }
        for ident in &self.identifiers {
            if let Some(scheme) = &ident.scheme {
                haystack.push_str(scheme);
                haystack.push(' ');
            }
            haystack.push_str(&ident.value);
            haystack.push('\n');
        }
        haystack.to_lowercase().contains(&query)
    }

    fn sort_author(&self) -> Option<&str> {
        self.author_sort
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| self.authors.first().map(String::as_str))
    }
}

fn cmp_text(a: &str, b: &str, reverse: bool) -> Ordering {
    let ordering = a.to_lowercase().cmp(&b.to_lowercase());
    if reverse {
        ordering.reverse()
    } else {
        ordering
    }
}

fn cmp_opt_text(a: Option<&str>, b: Option<&str>, reverse: bool) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        // Missing values stay at the end regardless of sort direction.
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(a), Some(b)) => cmp_text(a, b, reverse),
    }
}

fn cmp_opt<T: PartialOrd>(a: Option<T>, b: Option<T>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(a), Some(b)) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
    }
}

fn cmp_opt_reverse<T: PartialOrd>(a: Option<T>, b: Option<T>, reverse: bool) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        // Missing values stay at the end regardless of sort direction.
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(a), Some(b)) => {
            let ordering = a.partial_cmp(&b).unwrap_or(Ordering::Equal);
            if reverse {
                ordering.reverse()
            } else {
                ordering
            }
        }
    }
}

/// Compare two books by a metadata field. Missing metadata sorts last.
pub fn compare_books(a: &Book, b: &Book, key: SortKey, reverse: bool) -> Ordering {
    let primary = match key {
        SortKey::Recent => {
            let ordering = a.recent_timestamp().cmp(&b.recent_timestamp());
            if reverse {
                ordering
            } else {
                ordering.reverse()
            }
        }
        SortKey::Title => cmp_text(&a.title, &b.title, reverse),
        SortKey::Author => cmp_opt_text(a.sort_author(), b.sort_author(), reverse),
        SortKey::Series => cmp_opt_text(a.series.as_deref(), b.series.as_deref(), reverse)
            .then_with(|| {
                // Keep the order within a series ascending even when the series
                // names themselves are shown descending.
                cmp_opt(a.series_index, b.series_index)
            }),
        SortKey::Published => cmp_opt_reverse(a.year(), b.year(), reverse)
            .then_with(|| cmp_opt_text(a.published.as_deref(), b.published.as_deref(), reverse)),
        SortKey::Added => {
            let ordering = a.added_at.cmp(&b.added_at);
            if reverse {
                ordering.reverse()
            } else {
                ordering
            }
        }
    };
    // Recent ties usually mean several books share a timestamp; alphabetical
    // order is more useful than reversing the title along with the dates.
    let tie_reverse = if key == SortKey::Recent {
        false
    } else {
        reverse
    };
    primary
        .then_with(|| cmp_text(&a.title, &b.title, tie_reverse))
        .then_with(|| a.id.cmp(&b.id))
}

/// Indices into `books`, filtered by `query` and sorted by `key`.
pub fn sorted_indices(
    books: &[Book],
    key: SortKey,
    reverse: bool,
    query: Option<&str>,
) -> Vec<usize> {
    let mut indices: Vec<usize> = books
        .iter()
        .enumerate()
        .filter(|(_, book)| query.is_none_or(|q| book.matches(q)))
        .map(|(index, _)| index)
        .collect();
    indices.sort_by(|&a, &b| compare_books(&books[a], &books[b], key, reverse));
    indices
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(
        title: &str,
        author: &str,
        series: Option<&str>,
        index: Option<f32>,
        date: &str,
    ) -> Book {
        Book {
            id: format!("id{title}"),
            hash: String::new(),
            title: title.to_string(),
            author_sort: None,
            authors: vec![author.to_string()],
            series: series.map(str::to_string),
            series_index: index,
            publisher: None,
            published: Some(date.to_string()),
            language: None,
            description: None,
            subjects: Vec::new(),
            identifiers: Vec::new(),
            epub_path: "books/x/book.epub".to_string(),
            cover_path: None,
            added_at: 0,
            last_opened_at: None,
            size: 0,
            reading_progress: None,
        }
    }

    #[test]
    fn sorts_by_author_and_year() {
        let books = vec![
            book("Zed", "Alice", None, None, "2001"),
            book("Aaa", "Bob", None, None, "1999"),
            book("Mid", "Alice", None, None, "2010"),
        ];
        let by_author = sorted_indices(&books, SortKey::Author, false, None);
        assert_eq!(by_author, vec![2, 0, 1]);
        let by_year = sorted_indices(&books, SortKey::Published, true, None);
        assert_eq!(by_year, vec![2, 0, 1]);
    }

    #[test]
    fn missing_metadata_sorts_last_in_both_directions() {
        let mut books = vec![
            book("With series", "A", Some("Saga"), Some(2.0), "2000"),
            book("No series", "B", None, None, ""),
            book("First", "C", Some("Saga"), Some(1.0), "1990"),
        ];
        books[2].published = None;
        let ascending = sorted_indices(&books, SortKey::Series, false, None);
        assert_eq!(ascending, vec![2, 0, 1]);
        let descending = sorted_indices(&books, SortKey::Series, true, None);
        assert_eq!(descending.last(), Some(&1));
        let missing_date = sorted_indices(&books, SortKey::Published, false, None);
        assert_eq!(missing_date.last(), Some(&2));
        let missing_date_desc = sorted_indices(&books, SortKey::Published, true, None);
        assert_eq!(missing_date_desc.last(), Some(&2));
    }

    #[test]
    fn sorts_by_recent_activity() {
        let mut books = vec![
            book("Added Old", "A", None, None, "1990"),
            book("Opened", "B", None, None, "1990"),
            book("Read", "C", None, None, "1990"),
            book("Untouched", "D", None, None, "1990"),
        ];
        books[0].added_at = 300;
        books[1].added_at = 100;
        books[1].last_opened_at = Some(500);
        books[2].added_at = 100;
        books[2].reading_progress = Some(ReadingProgress {
            chapter: 0,
            chapters: 1,
            fraction: 0.5,
            updated_at: 700,
        });

        let recent = sorted_indices(&books, SortKey::Recent, false, None);
        assert_eq!(recent, vec![2, 1, 0, 3]);
        assert!(SortKey::Recent.descending(false));
        assert_eq!(SortKey::Recent.arrow(false), "↓");

        let oldest = sorted_indices(&books, SortKey::Recent, true, None);
        assert_eq!(oldest, vec![3, 0, 1, 2]);
        assert!(!SortKey::Recent.descending(true));
        assert_eq!(SortKey::Recent.arrow(true), "↑");

        // Added and opened still win when they are the newest activity.
        books[1].last_opened_at = Some(900);
        let recent = sorted_indices(&books, SortKey::Recent, false, None);
        assert_eq!(recent[0], 1);
    }

    #[test]
    fn recent_ties_break_alphabetically() {
        let books = vec![
            book("Zebra", "A", None, None, "1990"),
            book("Apple", "B", None, None, "1990"),
        ];
        let recent = sorted_indices(&books, SortKey::Recent, false, None);
        assert_eq!(recent, vec![1, 0]);
        let recent_reversed = sorted_indices(&books, SortKey::Recent, true, None);
        assert_eq!(recent_reversed, vec![1, 0]);
    }

    #[test]
    fn search_matches_metadata() {
        let mut b = book("The Hobbit", "Tolkien", None, None, "1937");
        b.subjects = vec!["Fantasy".into()];
        assert!(b.matches("hobbit"));
        assert!(b.matches("FANTASY"));
        assert!(b.matches("tolkien"));
        assert!(!b.matches("dune"));
    }

    #[test]
    fn computes_whole_book_progress() {
        let start = ReadingProgress {
            chapter: 0,
            chapters: 4,
            fraction: 0.0,
            updated_at: 0,
        };
        assert_eq!(start.percent(), 0);

        let half = ReadingProgress {
            chapter: 1,
            chapters: 4,
            fraction: 0.5,
            updated_at: 0,
        };
        assert!((half.overall_fraction() - 0.375).abs() < f64::EPSILON);
        assert_eq!(half.percent(), 38);

        let end = ReadingProgress {
            chapter: 3,
            chapters: 4,
            fraction: 1.0,
            updated_at: 0,
        };
        assert_eq!(end.percent(), 100);

        let clamped = ReadingProgress {
            chapter: 9,
            chapters: 2,
            fraction: 2.0,
            updated_at: 0,
        };
        assert_eq!(clamped.percent(), 100);

        let empty = ReadingProgress {
            chapter: 0,
            chapters: 0,
            fraction: 0.5,
            updated_at: 0,
        };
        assert_eq!(empty.percent(), 0);
    }
}
