use std::path::Path;

use anyhow::{Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthChar;

use crate::epub::{
    Block as BookBlock, Chapter, EpubBook, InlineSpan, InlineStyle, TocEntry, read_book,
};
use crate::model::ReadingProgress;
use crate::util;

const MAX_COLUMN_WIDTH: u16 = 76;
const MIN_COLUMN_WIDTH: u16 = 48;
const MIN_ADJUSTED_COLUMN_WIDTH: u16 = 20;
const COLUMN_WIDTH_STEP: i16 = 4;
const COLUMN_GUTTER: u16 = 4;
const OUTER_MARGIN: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnMode {
    Auto,
    One,
    Two,
}

impl ColumnMode {
    fn next(self) -> Self {
        match self {
            Self::Auto => Self::One,
            Self::One => Self::Two,
            Self::Two => Self::Auto,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::One => "one",
            Self::Two => "two",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderAction {
    None,
    Close,
}

struct CachedChapter {
    width: u16,
    lines: Vec<Line<'static>>,
}

/// Read-only, page-oriented EPUB reader used by the interactive browser.
pub struct Reader {
    book: EpubBook,
    chapter: usize,
    page: usize,
    pages: usize,
    restore: Option<f64>,
    mode: ColumnMode,
    columns: usize,
    rows: usize,
    col_width: u16,
    col_width_target: Option<u16>,
    screen: Rect,
    cache: Vec<Option<CachedChapter>>,
    toc_open: bool,
    toc_selection: usize,
    help_open: bool,
}

impl Reader {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_at(path, None)
    }

    pub fn open_at(path: &Path, progress: Option<ReadingProgress>) -> Result<Self> {
        Self::from_book_at(read_book(path)?, progress)
    }

    pub fn from_book(book: EpubBook) -> Result<Self> {
        Self::from_book_at(book, None)
    }

    pub fn from_book_at(book: EpubBook, progress: Option<ReadingProgress>) -> Result<Self> {
        if book.chapters.is_empty() {
            bail!("this EPUB does not contain any readable text");
        }
        let chapter = progress
            .map(|progress| progress.chapter.min(book.chapters.len() - 1))
            .unwrap_or(0);
        let restore = progress.map(|progress| progress.fraction.clamp(0.0, 1.0));
        let cache = (0..book.chapters.len()).map(|_| None).collect();
        Ok(Self {
            book,
            chapter,
            page: 0,
            pages: 1,
            restore,
            mode: ColumnMode::Auto,
            columns: 0,
            rows: 0,
            col_width: 0,
            col_width_target: None,
            screen: Rect::new(0, 0, 80, 24),
            cache,
            toc_open: false,
            toc_selection: 0,
            help_open: false,
        })
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ReaderAction {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
        {
            return ReaderAction::Close;
        }

        if self.help_open {
            self.help_open = false;
            return ReaderAction::None;
        }

        if self.toc_open {
            return self.handle_toc_key(key);
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return ReaderAction::Close,
            KeyCode::Char('j')
            | KeyCode::Char('l')
            | KeyCode::Down
            | KeyCode::Right
            | KeyCode::Char(' ')
            | KeyCode::PageDown => {
                self.next_page();
            }
            KeyCode::Char('k')
            | KeyCode::Char('h')
            | KeyCode::Up
            | KeyCode::Left
            | KeyCode::PageUp => self.previous_page(),
            KeyCode::Char('g') | KeyCode::Home => {
                self.relayout(self.screen);
                self.page = 0;
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.relayout(self.screen);
                self.page = self.pages.saturating_sub(1);
            }
            KeyCode::Char(']') => self.next_chapter(),
            KeyCode::Char('[') => self.previous_chapter(),
            KeyCode::Char('t') => self.open_toc(),
            KeyCode::Char('c') => {
                self.mode = self.mode.next();
                self.relayout(self.screen);
            }
            KeyCode::Char('+') | KeyCode::Char('=') => self.adjust_column_width(COLUMN_WIDTH_STEP),
            KeyCode::Char('-') | KeyCode::Char('_') => {
                self.adjust_column_width(-COLUMN_WIDTH_STEP);
            }
            KeyCode::Char('0') => self.reset_column_width(),
            KeyCode::Char('?') => self.help_open = true,
            _ => {}
        }
        ReaderAction::None
    }

    fn handle_toc_key(&mut self, key: KeyEvent) -> ReaderAction {
        let visible = self.toc_visible();
        match key.code {
            KeyCode::Esc | KeyCode::Char('t') | KeyCode::Char('q') => {
                self.toc_open = false;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.book.toc.is_empty() {
                    self.toc_selection =
                        (self.toc_selection + 1).min(self.book.toc.len().saturating_sub(1));
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.toc_selection = self.toc_selection.saturating_sub(1);
            }
            KeyCode::PageDown | KeyCode::Char(' ') => {
                if !self.book.toc.is_empty() {
                    self.toc_selection =
                        (self.toc_selection + visible).min(self.book.toc.len() - 1);
                }
            }
            KeyCode::PageUp => {
                self.toc_selection = self.toc_selection.saturating_sub(visible);
            }
            KeyCode::Char('g') | KeyCode::Home => self.toc_selection = 0,
            KeyCode::Char('G') | KeyCode::End => {
                self.toc_selection = self.book.toc.len().saturating_sub(1);
            }
            KeyCode::Enter => {
                if let Some(entry) = self.book.toc.get(self.toc_selection) {
                    self.chapter = entry.chapter;
                    self.page = 0;
                    self.refresh_pages();
                }
                self.toc_open = false;
            }
            _ => {}
        }
        ReaderAction::None
    }

    fn toc_visible(&self) -> usize {
        (self.screen.height.saturating_sub(6) as usize)
            .min(self.book.toc.len().max(1))
            .max(1)
    }

    fn open_toc(&mut self) {
        self.toc_open = true;
        self.toc_selection = self
            .book
            .toc
            .iter()
            .position(|entry| entry.chapter >= self.chapter)
            .unwrap_or(0);
    }

    fn next_page(&mut self) {
        self.relayout(self.screen);
        if self.page + 1 < self.pages {
            self.page += 1;
        } else if self.chapter + 1 < self.book.chapters.len() {
            self.chapter += 1;
            self.page = 0;
            self.refresh_pages();
        }
    }

    fn previous_page(&mut self) {
        self.relayout(self.screen);
        if self.page > 0 {
            self.page -= 1;
        } else if self.chapter > 0 {
            self.chapter -= 1;
            self.pages = self.page_count();
            self.page = self.pages.saturating_sub(1);
        }
    }

    fn next_chapter(&mut self) {
        if self.chapter + 1 < self.book.chapters.len() {
            self.chapter += 1;
            self.page = 0;
            self.refresh_pages();
        }
    }

    fn previous_chapter(&mut self) {
        if self.chapter > 0 {
            self.chapter -= 1;
            self.page = 0;
            self.refresh_pages();
        }
    }

    fn refresh_pages(&mut self) {
        self.pages = self.page_count();
        self.page = self.page.min(self.pages.saturating_sub(1));
    }

    fn page_count(&mut self) -> usize {
        let rows = self.rows.max(1);
        let columns = self.columns.max(1);
        let lines = self.chapter_lines().len();
        if lines == 0 {
            1
        } else {
            lines.div_ceil(rows * columns)
        }
    }

    fn chapter_lines(&mut self) -> &[Line<'static>] {
        let width = self.col_width.max(1);
        let stale = self.cache[self.chapter]
            .as_ref()
            .is_none_or(|cached| cached.width != width);
        if stale {
            let lines = layout_chapter(&self.book.chapters[self.chapter], width);
            self.cache[self.chapter] = Some(CachedChapter { width, lines });
        }
        &self.cache[self.chapter]
            .as_ref()
            .expect("chapter cache is populated")
            .lines
    }

    fn adjust_column_width(&mut self, delta: i16) {
        let current = self
            .col_width_target
            .unwrap_or(self.col_width)
            .clamp(MIN_ADJUSTED_COLUMN_WIDTH, MAX_COLUMN_WIDTH);
        let width = (current as i32 + delta as i32)
            .clamp(MIN_ADJUSTED_COLUMN_WIDTH as i32, MAX_COLUMN_WIDTH as i32)
            as u16;
        self.col_width_target = Some(width);
        self.relayout(self.screen);
    }

    fn reset_column_width(&mut self) {
        self.col_width_target = None;
        self.relayout(self.screen);
    }

    fn relayout(&mut self, area: Rect) {
        let (_, body, _) = reader_regions(area);
        let (columns, col_width) = column_plan_with(body.width, self.mode, self.col_width_target);
        let rows = body.height.max(1) as usize;

        if columns == self.columns && rows == self.rows && col_width == self.col_width {
            return;
        }

        let old_pages = self.pages.max(1);
        let progress = if old_pages > 1 {
            self.page as f32 / (old_pages - 1) as f32
        } else {
            0.0
        };

        self.columns = columns.max(1);
        self.rows = rows;
        self.col_width = col_width.max(1);
        self.pages = self.page_count();
        self.page = match self.restore.take() {
            Some(fraction) => ((fraction * self.pages.saturating_sub(1) as f64).round() as usize)
                .min(self.pages.saturating_sub(1)),
            None => ((progress * self.pages.saturating_sub(1) as f32).round() as usize)
                .min(self.pages.saturating_sub(1)),
        };
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        self.screen = area;
        self.relayout(area);
        let (header, body, footer) = reader_regions(area);

        self.draw_header(frame, header);
        self.draw_body(frame, body);
        self.draw_footer(frame, footer);

        if self.toc_open {
            self.draw_toc(frame, area);
        }
        if self.help_open {
            draw_help(frame, area);
        }
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        if area.height == 0 {
            return;
        }
        let book = self.book.title.as_deref().unwrap_or("Untitled");
        let chapter = self
            .book
            .chapters
            .get(self.chapter)
            .and_then(|chapter| chapter.title.as_deref())
            .unwrap_or("Untitled chapter");
        let line = Line::from(vec![
            Span::styled(
                format!(" {} ", util::truncate_chars(book, 32)),
                Style::default()
                    .fg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("· {} ", util::truncate_chars(chapter, 40)),
                Style::default().fg(Color::Gray),
            ),
            Span::styled(
                format!(
                    "· {} · {} col{} · {}w · page {}/{} · {}% ",
                    self.mode.label(),
                    self.columns,
                    if self.columns == 1 { "" } else { "s" },
                    self.col_width,
                    self.page + 1,
                    self.pages,
                    self.current_progress().percent()
                ),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        frame.render_widget(Paragraph::new(line), area);
    }

    fn draw_body(&mut self, frame: &mut Frame, body: Rect) {
        if body.height == 0 || body.width == 0 {
            return;
        }
        let rows = self.rows.max(1);
        let columns = self.columns.max(1);
        let col_width = self.col_width.max(1);
        let total = col_width * columns as u16 + COLUMN_GUTTER * columns.saturating_sub(1) as u16;
        let start_x = body.x + body.width.saturating_sub(total) / 2;
        let start = self.page * rows * columns;

        let lines = self.chapter_lines();
        for column in 0..columns {
            let x = start_x + column as u16 * (col_width + COLUMN_GUTTER);
            let rect = Rect {
                x,
                y: body.y,
                width: col_width.min(body.width),
                height: body.height,
            };
            let first = start + column * rows;
            let last = (first + rows).min(lines.len());
            let visible = if first < last {
                lines[first..last].to_vec()
            } else {
                Vec::new()
            };
            frame.render_widget(Paragraph::new(Text::from(visible)), rect);
        }
    }

    fn draw_footer(&self, frame: &mut Frame, area: Rect) {
        if area.height == 0 {
            return;
        }
        let line = Line::from(Span::styled(
            " j/k page · PgDn/PgUp · [ ] chapter · t contents · c columns · +/- width · ? help · q back",
            Style::default().fg(Color::DarkGray),
        ));
        frame.render_widget(Paragraph::new(line), area);
    }

    fn draw_toc(&mut self, frame: &mut Frame, area: Rect) {
        if self.book.toc.is_empty() {
            return;
        }
        let visible = self.toc_visible();
        let height = (visible as u16 + 2).min(area.height);
        let width = area.width.saturating_sub(8).clamp(20, 78);
        let popup = centered_rect(area, width, height);
        frame.render_widget(Clear, popup);

        let offset = self.toc_selection.saturating_sub(visible.saturating_sub(1));
        let mut lines = Vec::new();
        for (index, entry) in self.book.toc.iter().enumerate().skip(offset).take(visible) {
            let selected = index == self.toc_selection;
            let indent = "  ".repeat(entry.depth);
            let marker = if selected { "▶ " } else { "  " };
            let line = format!(
                "{marker}{indent}{}",
                util::truncate_chars(&entry.title, width.saturating_sub(6) as usize)
            );
            let style = if selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(line, style)));
        }

        let paragraph = Paragraph::new(Text::from(lines)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Contents ")
                .border_style(Style::default().fg(Color::Cyan)),
        );
        frame.render_widget(paragraph, popup);
    }

    pub fn page(&self) -> usize {
        self.page
    }

    pub fn pages(&self) -> usize {
        self.pages
    }

    pub fn columns(&self) -> usize {
        self.columns
    }

    pub fn chapter(&self) -> usize {
        self.chapter
    }

    /// Current position within the book, as persisted in the library index.
    pub fn position(&mut self) -> ReadingProgress {
        self.relayout(self.screen);
        self.current_progress()
    }

    fn current_progress(&self) -> ReadingProgress {
        let fraction = if self.pages > 1 {
            self.page as f64 / (self.pages - 1) as f64
        } else {
            // A one-page chapter is fully visible, so it counts as complete.
            1.0
        };
        ReadingProgress {
            chapter: self.chapter,
            chapters: self.book.chapters.len(),
            fraction,
            updated_at: 0,
        }
    }

    pub fn toc(&self) -> &[TocEntry] {
        &self.book.toc
    }

    pub fn set_screen(&mut self, area: Rect) {
        self.screen = area;
    }
}

pub fn column_plan(body_width: u16, mode: ColumnMode) -> (usize, u16) {
    column_plan_with(body_width, mode, None)
}

/// Plan the column layout, honoring an optional user-requested column width.
pub fn column_plan_with(
    body_width: u16,
    mode: ColumnMode,
    target_width: Option<u16>,
) -> (usize, u16) {
    let usable = body_width.max(1);
    let target = target_width.map(|width| width.clamp(MIN_ADJUSTED_COLUMN_WIDTH, MAX_COLUMN_WIDTH));
    let two_min = MIN_COLUMN_WIDTH * 2 + COLUMN_GUTTER;
    let want_two = match mode {
        ColumnMode::Auto => usable >= two_min,
        ColumnMode::One => false,
        ColumnMode::Two => usable >= two_min,
    };
    if want_two {
        let available = ((usable - COLUMN_GUTTER) / 2).min(MAX_COLUMN_WIDTH);
        let width = target.map_or(available, |width| width.min(available));
        (2, width.max(1))
    } else {
        let width = target.map_or(usable.min(MAX_COLUMN_WIDTH), |width| width.min(usable));
        (1, width.max(1))
    }
}

fn reader_regions(area: Rect) -> (Rect, Rect, Rect) {
    let header = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: area.height.min(1),
    };
    let footer = if area.height >= 2 {
        Rect {
            x: area.x,
            y: area.y + area.height - 1,
            width: area.width,
            height: 1,
        }
    } else {
        Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 0,
        }
    };
    let top = area.y + if area.height >= 5 { 2 } else { 1 };
    let bottom = if area.height >= 2 {
        footer.y
    } else {
        area.y + area.height
    };
    let margin = OUTER_MARGIN.min(area.width / 4);
    let body = Rect {
        x: area.x + margin,
        y: top.min(area.y + area.height),
        width: area.width.saturating_sub(margin * 2),
        height: bottom.saturating_sub(top),
    };
    (header, body, footer)
}

fn draw_help(frame: &mut Frame, area: Rect) {
    let popup = centered_rect(area, area.width.saturating_sub(8).clamp(40, 66), 19);
    frame.render_widget(Clear, popup);
    let lines = vec![
        help_line("j / l / ↓ / → / Space", "next page"),
        help_line("k / h / ↑ / ←", "previous page"),
        help_line("PgDn / PgUp", "next / previous page"),
        help_line("g / Home", "start of chapter"),
        help_line("G / End", "end of chapter"),
        help_line("] / [", "next / previous chapter"),
        help_line("t", "table of contents"),
        help_line("c", "columns: auto → one → two"),
        help_line("+ / -", "wider / narrower columns"),
        help_line("0", "reset column width"),
        help_line("q / Esc", "back to the library (saves your place)"),
        Line::from(""),
        Line::from(Span::styled(
            "Book text always moves by whole pages.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "Press any key to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let help = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Reading keys ")
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(ratatui::widgets::Wrap { trim: true });
    frame.render_widget(help, popup);
}

fn help_line(key: &str, description: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {key:<17}"), Style::default().fg(Color::Yellow)),
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

fn layout_chapter(chapter: &Chapter, width: u16) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let has_heading = chapter
        .blocks
        .iter()
        .any(|block| matches!(block, BookBlock::Heading { .. }));
    if !has_heading && let Some(title) = &chapter.title {
        lines.push(Line::from(Span::styled(title.clone(), heading_style(1))));
        lines.push(Line::from(""));
    }

    let mut previous_was_list = false;
    for block in &chapter.blocks {
        let is_list = matches!(block, BookBlock::ListItem { .. });
        if !lines.is_empty() && !(previous_was_list && is_list) {
            push_blank(&mut lines);
        }
        match block {
            BookBlock::Heading { level, spans } => {
                let style = heading_style(*level);
                lines.extend(wrap_spans(spans, width, "", "", style));
            }
            BookBlock::Paragraph(spans) => {
                lines.extend(wrap_spans(spans, width, "", "", Style::default()));
            }
            BookBlock::Quote(spans) => {
                let style = Style::default().fg(Color::Gray);
                lines.extend(wrap_spans(spans, width, "│ ", "│ ", style));
            }
            BookBlock::ListItem {
                marker,
                depth,
                spans,
            } => {
                let indent = "  ".repeat(*depth);
                let prefix = format!("{indent}{marker} ");
                let rest = " ".repeat(text_width(&prefix));
                lines.extend(wrap_spans(spans, width, &prefix, &rest, Style::default()));
            }
            BookBlock::Preformatted(text) => {
                lines.extend(wrap_preformatted(text, width));
            }
            BookBlock::Rule => {
                lines.push(Line::from(Span::styled(
                    "─".repeat((width as usize).min(40)),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
        previous_was_list = is_list;
    }

    if lines.is_empty() {
        lines.push(Line::from(""));
    }
    lines
}

fn push_blank(lines: &mut Vec<Line<'static>>) {
    if lines.last().is_none_or(|line| line.width() > 0) {
        lines.push(Line::from(""));
    }
}

fn heading_style(level: u8) -> Style {
    if level == 1 {
        Style::default()
            .fg(Color::LightCyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

fn inline_style(style: InlineStyle) -> Style {
    match style {
        InlineStyle::Normal => Style::default(),
        InlineStyle::Bold => Style::default().add_modifier(Modifier::BOLD),
        InlineStyle::Italic => Style::default().add_modifier(Modifier::ITALIC),
        InlineStyle::BoldItalic => Style::default().add_modifier(Modifier::BOLD | Modifier::ITALIC),
        InlineStyle::Code => Style::default().fg(Color::LightCyan),
    }
}

fn wrap_spans(
    spans: &[InlineSpan],
    width: u16,
    first_prefix: &str,
    rest_prefix: &str,
    base: Style,
) -> Vec<Line<'static>> {
    let words = split_words(spans);
    if words.is_empty() {
        return Vec::new();
    }
    let width = width.max(1) as usize;
    let first_available = width.saturating_sub(text_width(first_prefix)).max(1);
    let rest_available = width.saturating_sub(text_width(rest_prefix)).max(1);
    let wrapped = wrap_words(words, first_available, rest_available);
    wrapped
        .into_iter()
        .enumerate()
        .map(|(index, pieces)| {
            let prefix = if index == 0 {
                first_prefix
            } else {
                rest_prefix
            };
            let mut spans = Vec::new();
            if !prefix.is_empty() {
                spans.push(Span::styled(
                    prefix.to_string(),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            for piece in pieces {
                spans.push(Span::styled(
                    piece.text,
                    inline_style(piece.style).patch(base),
                ));
            }
            Line::from(spans)
        })
        .collect()
}

fn wrap_preformatted(text: &str, width: u16) -> Vec<Line<'static>> {
    let width = width.max(1) as usize;
    let mut lines = Vec::new();
    for raw in text.lines() {
        let expanded = raw.replace('\t', "    ");
        let mut current = String::new();
        let mut used = 0;
        for ch in expanded.chars() {
            let char_width = char_width(ch);
            if used + char_width > width && used > 0 {
                lines.push(Line::from(Span::styled(
                    std::mem::take(&mut current),
                    Style::default().fg(Color::LightYellow),
                )));
                used = 0;
            }
            current.push(ch);
            used += char_width;
        }
        lines.push(Line::from(Span::styled(
            current,
            Style::default().fg(Color::LightYellow),
        )));
    }
    if lines.is_empty() {
        lines.push(Line::from(""));
    }
    lines
}

#[derive(Clone)]
struct Piece {
    text: String,
    style: InlineStyle,
}

#[derive(Clone, Default)]
struct Word {
    pieces: Vec<Piece>,
    width: usize,
}

fn split_words(spans: &[InlineSpan]) -> Vec<Word> {
    let mut words = Vec::new();
    let mut current = Word::default();
    for span in spans {
        for ch in span.text.chars() {
            if ch.is_whitespace() {
                if !current.pieces.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
                continue;
            }
            current.width += char_width(ch);
            if let Some(last) = current.pieces.last_mut()
                && last.style == span.style
            {
                last.text.push(ch);
            } else {
                current.pieces.push(Piece {
                    text: ch.to_string(),
                    style: span.style,
                });
            }
        }
    }
    if !current.pieces.is_empty() {
        words.push(current);
    }
    words
}

fn wrap_words(words: Vec<Word>, first_available: usize, rest_available: usize) -> Vec<Vec<Piece>> {
    let first_available = first_available.max(1);
    let rest_available = rest_available.max(1);
    let mut lines: Vec<Vec<Piece>> = Vec::new();
    let mut line: Vec<Piece> = Vec::new();
    let mut line_width = 0usize;
    let mut available = first_available;

    for mut word in words {
        loop {
            let separator = usize::from(!line.is_empty());
            if line_width + separator + word.width <= available {
                if separator == 1 {
                    line.push(Piece {
                        text: " ".to_string(),
                        style: word
                            .pieces
                            .first()
                            .map(|piece| piece.style)
                            .unwrap_or(InlineStyle::Normal),
                    });
                    line_width += 1;
                }
                line_width += word.width;
                line.extend(word.pieces);
                break;
            }

            if line.is_empty() {
                let (head, tail) = split_word(word, available);
                line.extend(head);
                lines.push(std::mem::take(&mut line));
                line_width = 0;
                available = rest_available;
                match tail {
                    Some(tail) => word = tail,
                    None => break,
                }
            } else {
                lines.push(std::mem::take(&mut line));
                line_width = 0;
                available = rest_available;
            }
        }
    }

    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(Vec::new());
    }
    lines
}

fn split_word(word: Word, available: usize) -> (Vec<Piece>, Option<Word>) {
    let mut head = Vec::new();
    let mut used = 0usize;
    let mut rest = Vec::new();
    for piece in word.pieces {
        for ch in piece.text.chars() {
            if rest.is_empty() && used < available {
                let width = char_width(ch);
                if used + width <= available {
                    used += width;
                    head.push((ch, piece.style));
                    continue;
                }
                if head.is_empty() {
                    // A single wide character can be wider than a very narrow
                    // column. Keep it anyway so wrapping always makes progress.
                    head.push((ch, piece.style));
                    used += width;
                    continue;
                }
            }
            rest.push((ch, piece.style));
        }
    }
    let head = chars_to_pieces(head);
    if rest.is_empty() {
        (head, None)
    } else {
        (head, Some(chars_to_word(rest)))
    }
}

fn chars_to_pieces(chars: Vec<(char, InlineStyle)>) -> Vec<Piece> {
    let mut pieces: Vec<Piece> = Vec::new();
    for (ch, style) in chars {
        if let Some(last) = pieces.last_mut()
            && last.style == style
        {
            last.text.push(ch);
        } else {
            pieces.push(Piece {
                text: ch.to_string(),
                style,
            });
        }
    }
    pieces
}

fn chars_to_word(chars: Vec<(char, InlineStyle)>) -> Word {
    let width = chars.iter().map(|(ch, _)| char_width(*ch)).sum();
    Word {
        pieces: chars_to_pieces(chars),
        width,
    }
}

fn char_width(ch: char) -> usize {
    ch.width().unwrap_or(0)
}

fn text_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::epub::{Block as BookBlock, Chapter, InlineSpan, InlineStyle, TocEntry};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn paragraph(text: &str) -> BookBlock {
        BookBlock::Paragraph(vec![InlineSpan {
            text: text.to_string(),
            style: InlineStyle::Normal,
        }])
    }

    fn sample_book() -> EpubBook {
        let mut blocks = vec![BookBlock::Heading {
            level: 1,
            spans: vec![InlineSpan {
                text: "Chapter One".to_string(),
                style: InlineStyle::Bold,
            }],
        }];
        for index in 1..=80 {
            blocks.push(paragraph(&format!(
                "Paragraph {index} has enough words to wrap across several terminal lines \
                 when the column is narrow."
            )));
        }
        let second = vec![
            BookBlock::Heading {
                level: 1,
                spans: vec![InlineSpan {
                    text: "Chapter Two".to_string(),
                    style: InlineStyle::Bold,
                }],
            },
            paragraph("The second chapter begins here."),
        ];
        EpubBook {
            title: Some("A Test Book".to_string()),
            authors: vec!["Test Author".to_string()],
            chapters: vec![
                Chapter {
                    title: Some("Chapter One".to_string()),
                    blocks,
                },
                Chapter {
                    title: Some("Chapter Two".to_string()),
                    blocks: second,
                },
            ],
            toc: vec![
                TocEntry {
                    title: "Chapter One".to_string(),
                    chapter: 0,
                    depth: 0,
                },
                TocEntry {
                    title: "Chapter Two".to_string(),
                    chapter: 1,
                    depth: 0,
                },
            ],
        }
    }

    fn press(reader: &mut Reader, code: KeyCode) {
        reader.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn render(reader: &mut Reader, width: u16, height: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| reader.draw(frame)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn column_plan_respects_widths() {
        assert_eq!(column_plan(76, ColumnMode::Auto), (1, 76));
        assert_eq!(column_plan(99, ColumnMode::Auto), (1, 76));
        assert_eq!(column_plan(100, ColumnMode::Auto), (2, 48));
        assert_eq!(column_plan(196, ColumnMode::Auto), (2, 76));
        assert_eq!(column_plan(76, ColumnMode::One), (1, 76));
        assert_eq!(column_plan(76, ColumnMode::Two), (1, 76));
        assert_eq!(column_plan(140, ColumnMode::Two), (2, 68));
        assert_eq!(column_plan_with(76, ColumnMode::One, Some(40)), (1, 40));
        assert_eq!(column_plan_with(140, ColumnMode::Two, Some(40)), (2, 40));
        assert_eq!(column_plan_with(30, ColumnMode::One, Some(76)), (1, 30));
        assert_eq!(
            column_plan_with(76, ColumnMode::One, Some(4)),
            (1, MIN_ADJUSTED_COLUMN_WIDTH)
        );
    }

    #[test]
    fn column_width_can_be_adjusted_and_reset() {
        let mut reader = Reader::from_book(sample_book()).unwrap();
        reader.set_screen(Rect::new(0, 0, 80, 24));
        reader.relayout(Rect::new(0, 0, 80, 24));
        assert_eq!(reader.columns(), 1);
        assert_eq!(reader.col_width, MAX_COLUMN_WIDTH);

        let pages = reader.pages();
        press(&mut reader, KeyCode::Char('-'));
        assert_eq!(
            reader.col_width,
            MAX_COLUMN_WIDTH - COLUMN_WIDTH_STEP as u16
        );
        assert!(reader.pages() >= pages);

        press(&mut reader, KeyCode::Char('+'));
        assert_eq!(reader.col_width, MAX_COLUMN_WIDTH);
        press(&mut reader, KeyCode::Char('+'));
        assert_eq!(reader.col_width, MAX_COLUMN_WIDTH);

        press(&mut reader, KeyCode::Char('-'));
        press(&mut reader, KeyCode::Char('0'));
        assert_eq!(reader.col_width, MAX_COLUMN_WIDTH);
        assert_eq!(reader.col_width_target, None);
    }

    #[test]
    fn adjusted_column_width_is_clamped_to_the_terminal() {
        let mut reader = Reader::from_book(sample_book()).unwrap();
        reader.set_screen(Rect::new(0, 0, 160, 30));
        reader.relayout(Rect::new(0, 0, 160, 30));
        assert_eq!(reader.columns(), 2);
        assert_eq!(reader.col_width, MAX_COLUMN_WIDTH);

        press(&mut reader, KeyCode::Char('-'));
        assert_eq!(reader.columns(), 2);
        assert_eq!(
            reader.col_width,
            MAX_COLUMN_WIDTH - COLUMN_WIDTH_STEP as u16
        );

        reader.set_screen(Rect::new(0, 0, 80, 24));
        reader.relayout(Rect::new(0, 0, 80, 24));
        assert_eq!(reader.columns(), 1);
        assert_eq!(
            reader.col_width,
            MAX_COLUMN_WIDTH - COLUMN_WIDTH_STEP as u16
        );
    }

    #[test]
    fn wraps_words_and_long_words_to_the_column() {
        let spans = vec![InlineSpan {
            text: "alpha beta gamma delta epsilon".to_string(),
            style: InlineStyle::Normal,
        }];
        let lines = wrap_spans(&spans, 12, "", "", Style::default());
        assert!(lines.len() > 1);
        for line in &lines {
            let width: usize = line
                .spans
                .iter()
                .map(|span| text_width(&span.content))
                .sum();
            assert!(width <= 12, "line is too wide: {width}");
        }

        let long = vec![InlineSpan {
            text: "supercalifragilisticexpialidocious".to_string(),
            style: InlineStyle::Normal,
        }];
        let lines = wrap_spans(&long, 8, "", "", Style::default());
        assert!(lines.len() > 1);
        for line in &lines {
            let width: usize = line
                .spans
                .iter()
                .map(|span| text_width(&span.content))
                .sum();
            assert!(width <= 8);
        }

        // Wide characters must still make progress in a one-cell column.
        let wide = vec![InlineSpan {
            text: "漢字".to_string(),
            style: InlineStyle::Normal,
        }];
        let lines = wrap_spans(&wide, 1, "", "", Style::default());
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn pages_move_by_whole_pages_and_respect_chapter_bounds() {
        let mut reader = Reader::from_book(sample_book()).unwrap();
        reader.set_screen(Rect::new(0, 0, 80, 24));
        reader.relayout(Rect::new(0, 0, 80, 24));
        assert_eq!(reader.columns(), 1);
        assert!(reader.pages() > 3);

        press(&mut reader, KeyCode::Char('j'));
        assert_eq!(reader.page(), 1);
        press(&mut reader, KeyCode::Right);
        assert_eq!(reader.page(), 2);
        press(&mut reader, KeyCode::Char('l'));
        assert_eq!(reader.page(), 3);
        press(&mut reader, KeyCode::Char(' '));
        assert_eq!(reader.page(), 4);
        press(&mut reader, KeyCode::Left);
        assert_eq!(reader.page(), 3);
        press(&mut reader, KeyCode::Char('h'));
        assert_eq!(reader.page(), 2);
        press(&mut reader, KeyCode::Char('k'));
        assert_eq!(reader.page(), 1);
        press(&mut reader, KeyCode::Char('G'));
        assert_eq!(reader.page(), reader.pages() - 1);
        press(&mut reader, KeyCode::Char('g'));
        assert_eq!(reader.page(), 0);

        press(&mut reader, KeyCode::Char(']'));
        assert_eq!(reader.chapter(), 1);
        assert_eq!(reader.page(), 0);
        press(&mut reader, KeyCode::Char('['));
        assert_eq!(reader.chapter(), 0);
    }

    #[test]
    fn page_turns_cross_chapter_boundaries() {
        let mut reader = Reader::from_book(sample_book()).unwrap();
        reader.set_screen(Rect::new(0, 0, 80, 24));
        reader.relayout(Rect::new(0, 0, 80, 24));

        press(&mut reader, KeyCode::Char('G'));
        assert_eq!(reader.chapter(), 0);
        assert_eq!(reader.page(), reader.pages() - 1);

        press(&mut reader, KeyCode::PageDown);
        assert_eq!(reader.chapter(), 1);
        assert_eq!(reader.page(), 0);

        press(&mut reader, KeyCode::PageUp);
        assert_eq!(reader.chapter(), 0);
        assert_eq!(reader.page(), reader.pages() - 1);

        // The final chapter does not wrap around.
        press(&mut reader, KeyCode::Char(']'));
        assert_eq!(reader.chapter(), 1);
        press(&mut reader, KeyCode::Char('G'));
        let last_page = reader.page();
        press(&mut reader, KeyCode::Char('j'));
        assert_eq!(reader.chapter(), 1);
        assert_eq!(reader.page(), last_page);
    }

    #[test]
    fn wide_terminals_use_two_columns_and_resize_keeps_progress() {
        let mut reader = Reader::from_book(sample_book()).unwrap();
        reader.set_screen(Rect::new(0, 0, 80, 24));
        reader.relayout(Rect::new(0, 0, 80, 24));
        let old_pages = reader.pages();
        reader.page = old_pages / 2;
        let progress = reader.page as f32 / (old_pages - 1) as f32;

        reader.set_screen(Rect::new(0, 0, 160, 30));
        reader.relayout(Rect::new(0, 0, 160, 30));
        assert_eq!(reader.columns(), 2);
        assert!(reader.pages() < old_pages);
        let expected = (progress * (reader.pages() - 1) as f32).round() as usize;
        assert_eq!(reader.page(), expected);

        press(&mut reader, KeyCode::Char('c'));
        assert_eq!(reader.columns(), 1);
        press(&mut reader, KeyCode::Char('c'));
        assert_eq!(reader.columns(), 2);
        press(&mut reader, KeyCode::Char('c'));
        assert_eq!(reader.columns(), 2);
    }

    #[test]
    fn page_turns_replace_the_whole_screen_not_a_line_off_the_top() {
        let mut reader = Reader::from_book(sample_book()).unwrap();
        let first = render(&mut reader, 80, 24);
        assert!(first.contains("Paragraph 1 "));
        press(&mut reader, KeyCode::Char('j'));
        let second = render(&mut reader, 80, 24);
        assert!(!second.contains("Paragraph 1 "));
        assert!(second.contains("Paragraph "));
    }

    #[test]
    fn wide_pages_show_two_columns() {
        let mut reader = Reader::from_book(sample_book()).unwrap();
        let screen = render(&mut reader, 160, 30);
        assert_eq!(reader.columns(), 2);
        assert!(screen.contains("Paragraph 1 "));
        assert!(screen.contains("Paragraph 7"));
    }

    #[test]
    fn restores_and_reports_saved_progress() {
        let progress = ReadingProgress {
            chapter: 0,
            chapters: 2,
            fraction: 0.5,
            updated_at: 0,
        };
        let mut reader = Reader::from_book_at(sample_book(), Some(progress)).unwrap();
        reader.set_screen(Rect::new(0, 0, 80, 24));
        reader.relayout(Rect::new(0, 0, 80, 24));

        assert_eq!(reader.chapter(), 0);
        assert!(reader.pages() > 1);
        let expected = (0.5 * (reader.pages() - 1) as f64).round() as usize;
        assert_eq!(reader.page(), expected);

        let saved = reader.position();
        assert_eq!(saved.chapter, 0);
        assert_eq!(saved.chapters, 2);
        let expected_fraction = reader.page() as f64 / (reader.pages() - 1) as f64;
        assert!((saved.fraction - expected_fraction).abs() < f64::EPSILON);

        let screen = render(&mut reader, 80, 24);
        assert!(screen.contains(&format!("{}%", saved.percent())));
    }

    #[test]
    fn clamps_an_out_of_range_saved_chapter() {
        let progress = ReadingProgress {
            chapter: 99,
            chapters: 2,
            fraction: 0.5,
            updated_at: 0,
        };
        let mut reader = Reader::from_book_at(sample_book(), Some(progress)).unwrap();
        reader.set_screen(Rect::new(0, 0, 80, 24));
        reader.relayout(Rect::new(0, 0, 80, 24));
        assert_eq!(reader.chapter(), 1);
    }

    #[test]
    fn renders_chapters_toc_and_help() {
        let mut reader = Reader::from_book(sample_book()).unwrap();
        let screen = render(&mut reader, 120, 30);
        assert!(screen.contains("A Test Book"));
        assert!(screen.contains("Chapter One"));
        assert!(screen.contains("Paragraph 1"));

        press(&mut reader, KeyCode::Char('t'));
        let screen = render(&mut reader, 120, 30);
        assert!(screen.contains("Contents"));
        assert!(screen.contains("Chapter Two"));
        press(&mut reader, KeyCode::Char('j'));
        press(&mut reader, KeyCode::Enter);
        assert_eq!(reader.chapter(), 1);
        assert!(render(&mut reader, 120, 30).contains("second chapter"));

        press(&mut reader, KeyCode::Char('?'));
        let screen = render(&mut reader, 120, 30);
        assert!(screen.contains("Reading keys"));
        assert!(screen.contains("Book text always moves by whole pages"));
        press(&mut reader, KeyCode::Esc);
        assert_ne!(
            reader.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ReaderAction::None
        );
    }
}
