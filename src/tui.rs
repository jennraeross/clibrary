use std::io::{self, IsTerminal};
use std::panic;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use crossterm::cursor;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Frame;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};

use crate::library::Library;
use crate::model::{Book, ReaderSettings, SortKey, sorted_indices};
use crate::reader::{Reader, ReaderAction};
use crate::util;
use crate::wrap;

const PAGE_SIZE: usize = 10;
const DETAILS_PAGE_LINES: usize = 5;

pub fn run(library: Library, sort: SortKey, reverse: bool) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("the interactive browser needs a terminal; try `clibrary list` instead");
    }

    install_panic_hook();
    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = ratatui::Terminal::new(backend).context("cannot start the terminal")?;
    terminal.clear().ok();

    let mut app = App::new(library, sort, reverse);
    let result = event_loop(&mut terminal, &mut app);
    // Best-effort save if the loop exited while a book was still open.
    app.close_reader();
    let _ = terminal.show_cursor();
    result
}

fn install_panic_hook() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous(info);
    }));
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("cannot enable terminal raw mode")?;
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen, cursor::Hide) {
            let _ = disable_raw_mode();
            return Err(error).context("cannot enter the alternate screen");
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, cursor::Show);
}

fn event_loop<B: Backend>(terminal: &mut ratatui::Terminal<B>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|frame| ui(frame, app))?;
        if app.quit {
            return Ok(());
        }
        if event::poll(Duration::from_millis(250)).context("cannot read terminal events")?
            && let Event::Key(key) = event::read().context("cannot read terminal events")?
            && key.kind == KeyEventKind::Press
        {
            app.handle_key(key)?;
        }
    }
}

struct App {
    library: Library,
    sort: SortKey,
    reverse: bool,
    entries: Vec<usize>,
    missing: Vec<bool>,
    table_state: TableState,
    search: String,
    draft: Option<String>,
    status: Option<(String, bool)>,
    confirm: Option<String>,
    help: bool,
    quit: bool,
    reader: Option<Reader>,
    reader_book: Option<String>,
    reader_column_width: Option<u16>,
    details_scroll: usize,
    details_focus: bool,
}

impl App {
    fn new(library: Library, sort: SortKey, reverse: bool) -> Self {
        let mut app = Self {
            library,
            sort,
            reverse,
            entries: Vec::new(),
            missing: Vec::new(),
            table_state: TableState::default(),
            search: String::new(),
            draft: None,
            status: None,
            confirm: None,
            help: false,
            quit: false,
            reader: None,
            reader_book: None,
            reader_column_width: None,
            details_scroll: 0,
            details_focus: false,
        };
        app.refresh_missing();
        app.rebuild();
        app
    }

    fn book_count(&self) -> usize {
        self.library.books().len()
    }

    fn missing_count(&self) -> usize {
        self.missing.iter().filter(|missing| **missing).count()
    }

    fn refresh_missing(&mut self) {
        self.missing = self
            .library
            .books()
            .iter()
            .map(|book| !self.library.exists(book))
            .collect();
    }

    fn active_filter(&self) -> &str {
        self.draft.as_deref().unwrap_or(&self.search)
    }

    fn rebuild(&mut self) {
        let previous = self.selected_book().map(|book| book.id.clone());
        let previous_position = self.table_state.selected().unwrap_or(0);
        let filter = self.active_filter().trim().to_lowercase();
        self.entries = sorted_indices(
            self.library.books(),
            self.sort,
            self.reverse,
            if filter.is_empty() {
                None
            } else {
                Some(&filter)
            },
        );

        let selection = previous
            .and_then(|id| {
                self.entries.iter().position(|&index| {
                    self.library
                        .books()
                        .get(index)
                        .is_some_and(|book| book.id == id)
                })
            })
            .or_else(|| {
                if self.entries.is_empty() {
                    None
                } else {
                    Some(previous_position.min(self.entries.len() - 1))
                }
            });
        self.table_state.select(selection);
        if self.entries.is_empty() {
            self.details_focus = false;
        }
        self.details_scroll = 0;
    }

    fn selected_book(&self) -> Option<&Book> {
        self.table_state
            .selected()
            .and_then(|position| self.entries.get(position))
            .and_then(|&index| self.library.books().get(index))
    }

    fn set_status(&mut self, message: impl Into<String>, error: bool) {
        self.status = Some((message.into(), error));
    }

    /// Save the open reader's place, then return to the library.
    fn close_reader(&mut self) {
        let Some(mut reader) = self.reader.take() else {
            return;
        };
        let progress = reader.position();
        let column_width = reader.column_width_target();
        let book_id = self.reader_book.take();
        let width_changed = column_width != self.reader_column_width;
        self.reader_column_width = column_width;
        if let Some(book_id) = book_id
            && let Err(error) = self.library.set_reading_progress(&book_id, progress)
        {
            self.set_status(format!("{error:#}"), true);
        }
        if width_changed
            && let Err(error) = self
                .library
                .set_reader_settings(ReaderSettings { column_width })
        {
            self.set_status(format!("{error:#}"), true);
        }
        // Saving reloads the index, so re-sync the browser's view of it.
        self.refresh_missing();
        self.rebuild();
    }

    fn move_selection(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let current = self.table_state.selected().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, self.entries.len() as isize - 1);
        if next != current {
            self.details_scroll = 0;
        }
        self.table_state.select(Some(next as usize));
    }

    fn scroll_details(&mut self, delta: isize) {
        self.details_scroll = self.details_scroll.saturating_add_signed(delta);
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        let ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'));

        if self.reader.is_some() {
            if ctrl_c {
                self.close_reader();
                self.quit = true;
                return Ok(());
            }
            let close = self
                .reader
                .as_mut()
                .is_some_and(|reader| reader.handle_key(key) == ReaderAction::Close);
            if close {
                self.close_reader();
            }
            return Ok(());
        }

        if ctrl_c {
            self.quit = true;
            return Ok(());
        }

        if self.help {
            self.help = false;
            return Ok(());
        }

        if let Some(book_id) = self.confirm.clone() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.confirm = None;
                    match self.library.remove(&book_id, false) {
                        Ok(removal) => {
                            let message = match &removal.warning {
                                Some(warning) => format!(
                                    "Removed \"{}\" — {}",
                                    util::truncate_chars(&removal.book.title, 50),
                                    warning
                                ),
                                None => format!(
                                    "Removed \"{}\"",
                                    util::truncate_chars(&removal.book.title, 60)
                                ),
                            };
                            self.set_status(message, removal.warning.is_some());
                            self.refresh_missing();
                            self.rebuild();
                        }
                        Err(error) => self.set_status(format!("{error:#}"), true),
                    }
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.confirm = None;
                    self.set_status("Delete cancelled", false);
                }
                _ => {}
            }
            return Ok(());
        }

        if self.draft.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.draft = None;
                    self.rebuild();
                }
                KeyCode::Enter => {
                    self.search = self.draft.take().unwrap_or_default();
                    self.rebuild();
                }
                KeyCode::Backspace => {
                    if let Some(draft) = self.draft.as_mut() {
                        draft.pop();
                    }
                    self.rebuild();
                }
                KeyCode::Delete => {
                    self.draft = Some(String::new());
                    self.rebuild();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.draft = Some(String::new());
                    self.rebuild();
                }
                KeyCode::Up => self.move_selection(-1),
                KeyCode::Down => self.move_selection(1),
                KeyCode::PageUp => self.move_selection(-(PAGE_SIZE as isize)),
                KeyCode::PageDown => self.move_selection(PAGE_SIZE as isize),
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    if let Some(draft) = self.draft.as_mut() {
                        draft.push(character);
                    }
                    self.rebuild();
                }
                _ => {}
            }
            return Ok(());
        }

        if self.details_focus {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => self.scroll_details(1),
                KeyCode::Char('k') | KeyCode::Up => self.scroll_details(-1),
                KeyCode::PageDown | KeyCode::Char(' ') => {
                    self.scroll_details(DETAILS_PAGE_LINES as isize);
                }
                KeyCode::PageUp => self.scroll_details(-(DETAILS_PAGE_LINES as isize)),
                KeyCode::Char('g') | KeyCode::Home => self.details_scroll = 0,
                KeyCode::Char('G') | KeyCode::End => self.details_scroll = usize::MAX,
                KeyCode::Tab | KeyCode::Esc => self.details_focus = false,
                KeyCode::Char('q') => self.quit = true,
                _ => {}
            }
            return Ok(());
        }

        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => {
                if self.search.is_empty() {
                    self.quit = true;
                } else {
                    self.search.clear();
                    self.rebuild();
                    self.set_status("Filter cleared", false);
                }
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('g') | KeyCode::Home => {
                if !self.entries.is_empty() {
                    self.table_state.select(Some(0));
                    self.details_scroll = 0;
                }
            }
            KeyCode::Char('G') | KeyCode::End => {
                if !self.entries.is_empty() {
                    self.table_state.select(Some(self.entries.len() - 1));
                    self.details_scroll = 0;
                }
            }
            KeyCode::PageDown => self.move_selection(PAGE_SIZE as isize),
            KeyCode::PageUp => self.move_selection(-(PAGE_SIZE as isize)),
            KeyCode::Tab => {
                if self.selected_book().is_some() {
                    self.details_focus = true;
                }
            }
            KeyCode::Char('/') => {
                self.draft = Some(self.search.clone());
            }
            KeyCode::Char('s') => {
                self.sort = self.sort.next();
                self.rebuild();
                self.set_status(format!("Sorted by {}", self.sort.label()), false);
            }
            KeyCode::Char('S') => {
                self.reverse = !self.reverse;
                self.rebuild();
                self.set_status(
                    format!(
                        "Sorted by {} ({})",
                        self.sort.label(),
                        self.sort.direction_label(self.reverse)
                    ),
                    false,
                );
            }
            KeyCode::Char('d') => {
                if let Some(book) = self.selected_book() {
                    self.confirm = Some(book.id.clone());
                }
            }
            KeyCode::Char('o') => {
                let Some(book) = self.selected_book().cloned() else {
                    return Ok(());
                };
                let path = self.library.book_path(&book);
                if !path.is_file() {
                    self.set_status(format!("File is missing: {}", path.display()), true);
                } else {
                    match util::open_in_viewer(&path) {
                        Ok(()) => {
                            self.set_status(format!("Opening {}", path.display()), false);
                            if let Err(error) = self.library.record_opened(&book.id) {
                                self.set_status(format!("{error:#}"), true);
                            }
                        }
                        Err(error) => self.set_status(format!("{error:#}"), true),
                    }
                }
            }
            KeyCode::Enter => {
                let Some(book) = self.selected_book().cloned() else {
                    return Ok(());
                };
                let path = self.library.book_path(&book);
                if !path.is_file() {
                    self.set_status(format!("File is missing: {}", path.display()), true);
                } else {
                    match Reader::open_at(&path, book.reading_progress) {
                        Ok(reader) => {
                            let column_width = self.library.reader_settings().column_width;
                            let record_error = self.library.record_opened(&book.id).err();
                            self.reader = Some(reader.with_column_width(column_width));
                            self.reader_book = Some(book.id);
                            self.reader_column_width = column_width;
                            self.status = record_error.map(|error| (format!("{error:#}"), true));
                        }
                        Err(error) => self.set_status(format!("{error:#}"), true),
                    }
                }
            }
            KeyCode::Char('r') => match self.library.reload() {
                Ok(()) => {
                    self.refresh_missing();
                    self.rebuild();
                    self.set_status(
                        format!("Reloaded {} books from disk", self.book_count()),
                        false,
                    );
                }
                Err(error) => self.set_status(format!("{error:#}"), true),
            },
            KeyCode::Char('?') => self.help = true,
            _ => {}
        }
        Ok(())
    }
}

fn ui(frame: &mut Frame, app: &mut App) {
    if let Some(reader) = app.reader.as_mut() {
        reader.draw(frame);
        return;
    }

    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(5),
            Constraint::Length(10),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(frame, app, chunks[0]);
    draw_table(frame, app, chunks[1]);
    draw_details(frame, app, chunks[2]);
    draw_footer(frame, app, chunks[3]);

    if app.help {
        draw_help(frame, area);
    }
    if app.confirm.is_some() {
        draw_confirm(frame, app, area);
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let filter = app.search.trim();
    let mut summary = format!(
        "{} book{} · sort: {} {}",
        app.book_count(),
        if app.book_count() == 1 { "" } else { "s" },
        app.sort.label(),
        app.sort.arrow(app.reverse)
    );
    let missing = app.missing_count();
    if missing > 0 {
        summary.push_str(&format!(" · {missing} missing"));
    }
    if !filter.is_empty() {
        summary.push_str(&format!(" · filter: {filter:?}"));
    }

    let status = match &app.status {
        Some((message, true)) => Line::from(Span::styled(
            format!(" {message}"),
            Style::default().fg(Color::LightRed),
        )),
        Some((message, false)) => Line::from(Span::styled(
            format!(" {message}"),
            Style::default().fg(Color::Green),
        )),
        None => Line::from(Span::styled(" Ready", Style::default().fg(Color::DarkGray))),
    };

    let lines = vec![
        Line::from(vec![
            Span::styled(
                " clibrary ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                app.library.root().display().to_string(),
                Style::default().fg(Color::Gray),
            ),
        ]),
        Line::from(vec![Span::styled(
            format!(" {summary}"),
            Style::default().fg(Color::Cyan),
        )]),
        status,
    ];

    let header = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(header, area);
}

fn draw_table(frame: &mut Frame, app: &mut App, area: Rect) {
    let rows: Vec<Row> = app
        .entries
        .iter()
        .map(|&index| {
            let book = &app.library.books()[index];
            let missing = app.missing.get(index).copied().unwrap_or(false);
            let title = if missing {
                format!("{} (missing)", book.title)
            } else {
                book.title.clone()
            };
            let series = book.series_display().unwrap_or_default();
            let year = book
                .year()
                .map(|year| year.to_string())
                .unwrap_or_else(|| "—".to_string());
            let style = if missing {
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM)
            } else {
                Style::default()
            };
            Row::new(vec![
                Cell::from(title),
                Cell::from(book.authors_display()),
                Cell::from(series),
                Cell::from(year),
            ])
            .style(style)
        })
        .collect();

    let header = Row::new(vec!["Title", "Author", "Series", "Year"]).style(
        Style::default()
            .fg(Color::LightCyan)
            .add_modifier(Modifier::BOLD),
    );
    let widths = [
        Constraint::Percentage(46),
        Constraint::Percentage(26),
        Constraint::Percentage(22),
        Constraint::Length(5),
    ];
    let title = if app.entries.len() == app.book_count() {
        format!(" Books ({}) ", app.book_count())
    } else {
        format!(" Books ({}/{}) ", app.entries.len(), app.book_count())
    };
    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(title),
        )
        .column_spacing(1)
        .row_highlight_style(
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(table, area, &mut app.table_state);
}

fn draw_details(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if app.details_focus {
            Color::Cyan
        } else {
            Color::DarkGray
        }))
        .title(if app.details_focus {
            " Details · scrolling "
        } else {
            " Details "
        });

    let lines = match app.selected_book() {
        Some(book) => {
            let mut lines = vec![
                Line::from(vec![
                    Span::styled(
                        book.title.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {}", book.id),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("by ", Style::default().fg(Color::DarkGray)),
                    Span::raw(book.authors_display()),
                ]),
            ];

            let mut facts = Vec::new();
            if let Some(series) = book.series_display() {
                facts.push(series);
            }
            if let Some(published) = &book.published {
                facts.push(published.clone());
            }
            if let Some(publisher) = &book.publisher {
                facts.push(publisher.clone());
            }
            if let Some(language) = &book.language {
                facts.push(language.clone());
            }
            if !facts.is_empty() {
                lines.push(Line::from(Span::styled(
                    facts.join(" · "),
                    Style::default().fg(Color::Gray),
                )));
            }

            if !book.subjects.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("Subjects: {}", book.subjects.join(", ")),
                    Style::default().fg(Color::Gray),
                )));
            }

            if let Some(progress) = &book.reading_progress {
                lines.push(Line::from(Span::styled(
                    format!(
                        "Reading: {}% · chapter {}/{} · last read {}",
                        progress.percent(),
                        progress.chapter.saturating_add(1),
                        progress.chapters.max(1),
                        util::format_timestamp(progress.updated_at)
                    ),
                    Style::default().fg(Color::LightGreen),
                )));
            }

            lines.push(Line::from(vec![
                Span::styled("File: ", Style::default().fg(Color::DarkGray)),
                Span::styled(book.epub_path.clone(), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("  {} · added {}", book.size_display(), book.added_display()),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));

            if let Some(description) = &book.description {
                lines.push(Line::from(""));
                lines.push(Line::from(util::truncate_chars(description, 4000)));
            }
            lines
        }
        None => vec![Line::from(Span::styled(
            if app.book_count() == 0 {
                "The library is empty. Import books with: clibrary add <file.epub>"
            } else {
                "No books match the current filter."
            },
            Style::default().fg(Color::DarkGray),
        ))],
    };

    let width = area.width.saturating_sub(2) as usize;
    let height = area.height.saturating_sub(2) as usize;
    let wrapped = wrap::wrap_lines(&lines, width);
    let max_scroll = wrapped.len().saturating_sub(height);
    app.details_scroll = app.details_scroll.min(max_scroll);

    let paragraph = Paragraph::new(Text::from(wrapped))
        .block(block)
        .scroll((app.details_scroll as u16, 0));
    frame.render_widget(paragraph, area);
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let line = if let Some(draft) = &app.draft {
        Line::from(vec![
            Span::styled(" Search: ", Style::default().fg(Color::Yellow)),
            Span::raw(draft.clone()),
            Span::styled("█", Style::default().fg(Color::Yellow)),
            Span::styled(
                "  Enter apply · Esc cancel",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    } else if app.details_focus {
        Line::from(Span::styled(
            " details: j/k scroll · PgDn/PgUp · g/G top/bottom · Tab/Esc back · q quit",
            Style::default().fg(Color::DarkGray),
        ))
    } else {
        let filter = if app.search.is_empty() {
            String::new()
        } else {
            format!("  [/] filter: {:?}", app.search)
        };
        Line::from(vec![
            Span::styled(
                " q quit · j/k · Tab details · / search · s sort · Enter read · o open · d delete · ? help",
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(filter, Style::default().fg(Color::Yellow)),
        ])
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_help(frame: &mut Frame, area: Rect) {
    let popup = centered_rect(area, 74, 24);
    frame.render_widget(Clear, popup);

    let lines = vec![
        Line::from(Span::styled(
            "Keyboard shortcuts",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled("Navigation", Style::default().fg(Color::Cyan))),
        help_line("j / ↓", "next book"),
        help_line("k / ↑", "previous book"),
        help_line("g / Home", "first book"),
        help_line("G / End", "last book"),
        help_line("PgDn / PgUp", "jump ten books"),
        help_line("Tab", "focus the details pane (j/k scroll, Esc/Tab back)"),
        Line::from(""),
        Line::from(Span::styled("Actions", Style::default().fg(Color::Cyan))),
        help_line("/", "search title, author, series, subjects, description"),
        help_line(
            "s",
            "cycle sort: recent, author, title, series, published, added",
        ),
        help_line("S", "reverse the sort order"),
        help_line("Enter", "read the selected EPUB inside clibrary"),
        help_line("o", "open the selected EPUB in your system viewer"),
        help_line("d", "remove the book and its files (asks first)"),
        help_line("r", "reload the index from disk"),
        help_line("q / Ctrl-C", "quit"),
        Line::from(""),
        Line::from(Span::styled(
            "Press any key to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let help = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Help ")
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(help, popup);
}

fn draw_confirm(frame: &mut Frame, app: &App, area: Rect) {
    let Some(book_id) = &app.confirm else {
        return;
    };
    let title = app
        .library
        .books()
        .iter()
        .find(|book| &book.id == book_id)
        .map(|book| book.title.clone())
        .unwrap_or_default();
    let popup = centered_rect(area, 70, 7);
    frame.render_widget(Clear, popup);
    let text = Text::from(vec![
        Line::from(vec![
            Span::styled("Delete ", Style::default().fg(Color::LightRed)),
            Span::styled(
                format!("\"{}\"", util::truncate_chars(&title, 46)),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(" and its files?", Style::default().fg(Color::LightRed)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "y / Enter confirm · n / Esc cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .alignment(Alignment::Center);
    let dialog = Paragraph::new(text)
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Confirm deletion ")
                .border_style(Style::default().fg(Color::LightRed)),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(dialog, popup);
}

fn help_line(key: &str, description: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {key:<15}"), Style::default().fg(Color::Yellow)),
        Span::raw(description.to_string()),
    ])
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{EpubSpec, write_epub};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn setup() -> (tempfile::TempDir, App) {
        let temporary = tempfile::tempdir().unwrap();
        let mut library = Library::open(temporary.path().join("library")).unwrap();
        for (title, author) in [("The Hobbit", "J.R.R. Tolkien"), ("Dune", "Frank Herbert")] {
            let source = temporary.path().join(format!("{title}.epub"));
            write_epub(&source, &EpubSpec::new(title, author)).unwrap();
            library.add_epub(&source).unwrap();
        }
        let app = App::new(library, SortKey::Author, false);
        (temporary, app)
    }

    fn setup_long_details() -> (tempfile::TempDir, App) {
        let temporary = tempfile::tempdir().unwrap();
        let mut library = Library::open(temporary.path().join("library")).unwrap();
        for (title, author, description) in [
            ("Long Book", "Author A", "word ".repeat(400)),
            ("Short Book", "Author B", "Short description.".to_string()),
        ] {
            let mut spec = EpubSpec::new(title, author);
            spec.description = Some(description);
            let source = temporary.path().join(format!("{title}.epub"));
            write_epub(&source, &spec).unwrap();
            library.add_epub(&source).unwrap();
        }
        let app = App::new(library, SortKey::Title, false);
        (temporary, app)
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
            .unwrap();
    }

    #[test]
    fn renders_books_table_and_details() {
        let (_temporary, mut app) = setup();
        let screen = render(&mut app, 110, 32);
        assert!(screen.contains("clibrary"));
        assert!(screen.contains("Books (2)"));
        assert!(screen.contains("The Hobbit"));
        assert!(screen.contains("J.R.R. Tolkien"));
        assert!(screen.contains("Dune"));
        assert!(screen.contains("Details"));
        // Default sort is author, so Dune (Herbert) is selected first.
        assert!(app.selected_book().is_some_and(|book| book.title == "Dune"));

        press(&mut app, KeyCode::Char('j'));
        assert!(
            app.selected_book()
                .is_some_and(|book| book.title == "The Hobbit")
        );
    }

    #[test]
    fn tab_focuses_and_scrolls_the_details_pane() {
        let (_temporary, mut app) = setup_long_details();
        let selected = app.selected_book().unwrap().id.clone();
        assert_eq!(app.details_scroll, 0);

        let before = render(&mut app, 100, 32);
        assert!(before.contains("Details"));
        assert!(before.contains("Long Book"));

        press(&mut app, KeyCode::Tab);
        assert!(app.details_focus);
        let top = render(&mut app, 100, 32);
        press(&mut app, KeyCode::Char('j'));
        let scrolled = render(&mut app, 100, 32);
        assert!(app.details_scroll > 0);
        assert_ne!(top, scrolled);
        assert_eq!(app.selected_book().unwrap().id, selected);

        press(&mut app, KeyCode::Char('G'));
        render(&mut app, 100, 32);
        assert!(app.details_scroll > 1);
        press(&mut app, KeyCode::Char('g'));
        render(&mut app, 100, 32);
        assert_eq!(app.details_scroll, 0);

        press(&mut app, KeyCode::Esc);
        assert!(!app.details_focus);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.details_scroll, 0);
        assert_ne!(app.selected_book().unwrap().id, selected);
    }

    #[test]
    fn filters_and_sorts() {
        let (_temporary, mut app) = setup();
        press(&mut app, KeyCode::Char('/'));
        for ch in "hobbit".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        let screen = render(&mut app, 110, 32);
        assert!(screen.contains("Books (1/2)"));
        assert!(screen.contains("The Hobbit"));
        assert!(!screen.contains("Dune"));

        press(&mut app, KeyCode::Enter);
        assert_eq!(app.search, "hobbit");
        assert_eq!(app.entries.len(), 1);

        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.entries.len(), 1);
        assert_eq!(app.search, "hobbit");

        press(&mut app, KeyCode::Char('/'));
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL))
            .unwrap();
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.entries.len(), 2);
        assert!(app.search.is_empty());

        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.sort, SortKey::Title);
        assert!(!app.reverse);
        assert!(render(&mut app, 110, 32).contains("Sorted by title"));
        press(&mut app, KeyCode::Char('S'));
        assert!(app.reverse);
    }

    #[test]
    fn shows_help_and_delete_dialog() {
        let (_temporary, mut app) = setup();
        press(&mut app, KeyCode::Char('?'));
        let screen = render(&mut app, 110, 32);
        assert!(screen.contains("Keyboard shortcuts"));

        press(&mut app, KeyCode::Esc);
        assert!(!app.help);

        press(&mut app, KeyCode::Char('d'));
        let screen = render(&mut app, 110, 32);
        assert!(screen.contains("Confirm deletion"));
        assert!(screen.contains("and its files?"));

        press(&mut app, KeyCode::Char('n'));
        assert!(app.confirm.is_none());
    }

    #[test]
    fn deletes_the_selected_book() {
        let (_temporary, mut app) = setup();
        let selected = app.selected_book().cloned().unwrap();
        let path = app.library.book_path(&selected);
        assert!(path.is_file());

        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('y'));

        assert_eq!(app.book_count(), 1);
        assert_eq!(app.entries.len(), 1);
        assert!(!path.exists());
        assert!(app.library.resolve("Dune").is_err());
    }

    #[test]
    fn opens_the_in_app_reader_and_returns() {
        let (_temporary, mut app) = setup();
        press(&mut app, KeyCode::Enter);
        assert!(app.reader.is_some());
        let screen = render(&mut app, 100, 30);
        assert!(screen.contains("Hello."));

        press(&mut app, KeyCode::Esc);
        assert!(app.reader.is_none());
        assert!(render(&mut app, 100, 30).contains("Books (2)"));
    }

    #[test]
    fn opening_a_book_records_the_open_time() {
        let (_temporary, mut app) = setup();
        let id = app.selected_book().unwrap().id.clone();
        assert!(app.library.resolve(&id).unwrap().last_opened_at.is_none());

        press(&mut app, KeyCode::Enter);
        assert!(app.reader.is_some());
        assert!(app.library.resolve(&id).unwrap().last_opened_at.is_some());

        press(&mut app, KeyCode::Esc);
    }

    #[test]
    fn saves_and_restores_the_reading_position() {
        let temporary = tempfile::tempdir().unwrap();
        let mut library = Library::open(temporary.path().join("library")).unwrap();
        let paragraphs: Vec<String> = (1..=120)
            .map(|index| {
                format!("Paragraph {index} has enough words to wrap across several terminal lines.")
            })
            .collect();
        let chapter = crate::fixtures::ChapterSpec::new(
            "Chapter One",
            &paragraphs.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let mut spec = EpubSpec::new("Long Book", "An Author");
        spec.chapters = vec![chapter];
        let source = temporary.path().join("long.epub");
        write_epub(&source, &spec).unwrap();
        library.add_epub(&source).unwrap();

        let mut app = App::new(library, SortKey::Title, false);
        render(&mut app, 100, 30);
        press(&mut app, KeyCode::Enter);
        assert!(app.reader.is_some());
        render(&mut app, 100, 30);

        for _ in 0..4 {
            press(&mut app, KeyCode::Char('j'));
        }
        let reader = app.reader.as_ref().unwrap();
        let chapter = reader.chapter();
        let page = reader.page();
        assert!(page > 0);

        press(&mut app, KeyCode::Esc);
        assert!(app.reader.is_none());
        let stored = app.library.books()[0].reading_progress.unwrap();
        assert_eq!(stored.chapter, chapter);
        assert!(stored.fraction > 0.0);

        press(&mut app, KeyCode::Enter);
        render(&mut app, 100, 30);
        let reader = app.reader.as_ref().unwrap();
        assert_eq!(reader.chapter(), chapter);
        assert_eq!(reader.page(), page);

        press(&mut app, KeyCode::Esc);
        let screen = render(&mut app, 100, 30);
        assert!(screen.contains("Reading:"));
    }

    #[test]
    fn remembers_the_column_width_between_sessions() {
        let (_temporary, mut app) = setup();
        render(&mut app, 100, 30);
        press(&mut app, KeyCode::Enter);
        render(&mut app, 100, 30);
        assert_eq!(app.reader.as_ref().unwrap().column_width_target(), None);

        press(&mut app, KeyCode::Char('-'));
        assert_eq!(app.reader.as_ref().unwrap().column_width_target(), Some(72));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.library.reader_settings().column_width, Some(72));

        // Reopening a book in the same session restores the saved width.
        press(&mut app, KeyCode::Enter);
        render(&mut app, 100, 30);
        assert_eq!(app.reader.as_ref().unwrap().column_width_target(), Some(72));
        press(&mut app, KeyCode::Esc);

        // A fresh process reads it back from the index.
        let root = app.library.root().to_path_buf();
        drop(app);
        let library = Library::open(root).unwrap();
        assert_eq!(library.reader_settings().column_width, Some(72));

        // `0` clears the saved preference.
        let mut app = App::new(library, SortKey::Title, false);
        render(&mut app, 100, 30);
        press(&mut app, KeyCode::Enter);
        render(&mut app, 100, 30);
        assert_eq!(app.reader.as_ref().unwrap().column_width_target(), Some(72));
        press(&mut app, KeyCode::Char('0'));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.library.reader_settings().column_width, None);
    }

    #[test]
    fn empty_library_shows_import_hint() {
        let temporary = tempfile::tempdir().unwrap();
        let library = Library::open(temporary.path().join("library")).unwrap();
        let mut app = App::new(library, SortKey::Title, false);
        let screen = render(&mut app, 100, 28);
        assert!(screen.contains("The library is empty"));
        assert!(screen.contains("clibrary add"));
    }

    #[test]
    fn missing_files_are_marked() {
        let (_temporary, mut app) = setup();
        let selected = app.selected_book().cloned().unwrap();
        std::fs::remove_file(app.library.book_path(&selected)).unwrap();
        app.refresh_missing();
        app.rebuild();
        let screen = render(&mut app, 110, 32);
        assert!(screen.contains("(missing)"));
        assert!(app.missing_count() == 1);
    }
}
