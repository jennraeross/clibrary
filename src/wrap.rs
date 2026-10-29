use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// Word-wrap styled lines to the given display width.
///
/// Blank lines are preserved so callers can use empty lines as spacing.
pub fn wrap_lines(lines: &[Line<'static>], width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut wrapped = Vec::new();
    for line in lines {
        wrap_line(line, width, &mut wrapped);
    }
    wrapped
}

fn wrap_line(line: &Line<'static>, width: usize, out: &mut Vec<Line<'static>>) {
    let tokens = tokenize(line);
    if tokens.is_empty() {
        out.push(Line::from("").style(line.style));
        return;
    }

    let mut current: Vec<Span<'static>> = Vec::new();
    let mut current_width = 0usize;
    let mut pending: Option<(String, Style)> = None;
    let mut pending_width = 0usize;

    for (text, style, is_space) in tokens {
        if is_space {
            pending_width += display_width(&text);
            match pending.as_mut() {
                Some((spaces, space_style)) if *space_style == style => spaces.push_str(&text),
                _ => pending = Some((text, style)),
            }
            continue;
        }

        let word_width = display_width(&text);
        if current_width > 0 && current_width + pending_width + word_width > width {
            out.push(Line::from(std::mem::take(&mut current)).style(line.style));
            current_width = 0;
            pending = None;
        }

        if word_width > width {
            // A word wider than the pane must be split so wrapping makes progress.
            pending = None;
            pending_width = 0;
            let mut rest = text.as_str();
            while !rest.is_empty() {
                let (head, tail) = split_at_width(rest, width);
                if tail.is_empty() {
                    current.push(Span::styled(head.to_string(), style));
                    current_width = display_width(head);
                } else {
                    out.push(
                        Line::from(vec![Span::styled(head.to_string(), style)]).style(line.style),
                    );
                }
                rest = tail;
            }
        } else {
            if current_width > 0
                && let Some((spaces, space_style)) = pending.take()
                && !spaces.is_empty()
            {
                current.push(Span::styled(spaces, space_style));
            }
            pending = None;
            pending_width = 0;
            current.push(Span::styled(text, style));
            current_width += word_width;
        }
    }

    if current.is_empty() {
        // A line made only of spaces still occupies a row.
        out.push(Line::from("").style(line.style));
    } else {
        out.push(Line::from(current).style(line.style));
    }
}

fn tokenize(line: &Line<'static>) -> Vec<(String, Style, bool)> {
    let mut tokens = Vec::new();
    for span in &line.spans {
        let mut text = String::new();
        let mut space: Option<bool> = None;
        for ch in span.content.chars() {
            let is_space = ch.is_whitespace();
            if let Some(previous) = space
                && previous != is_space
            {
                tokens.push((std::mem::take(&mut text), span.style, previous));
            }
            space = Some(is_space);
            text.push(ch);
        }
        if let Some(previous) = space {
            tokens.push((text, span.style, previous));
        }
    }
    tokens
}

fn split_at_width(text: &str, width: usize) -> (&str, &str) {
    let mut used = 0usize;
    for (index, ch) in text.char_indices() {
        let char_width = ch.width().unwrap_or(0);
        if used + char_width > width {
            if index == 0 {
                let end = ch.len_utf8();
                return (&text[..end], &text[end..]);
            }
            return (&text[..index], &text[index..]);
        }
        used += char_width;
    }
    (text, "")
}

fn display_width(text: &str) -> usize {
    text.chars().map(|ch| ch.width().unwrap_or(0)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> Line<'static> {
        Line::from(Span::raw(text.to_string()))
    }

    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn wraps_on_word_boundaries() {
        let lines = vec![plain("alpha beta gamma delta")];
        let wrapped = wrap_lines(&lines, 10);
        assert_eq!(
            wrapped.iter().map(line_text).collect::<Vec<_>>(),
            vec!["alpha beta", "gamma", "delta"]
        );
    }

    #[test]
    fn preserves_blank_lines_and_styles() {
        let lines = vec![
            Line::from(vec![
                Span::styled(
                    "bold",
                    Style::default().add_modifier(ratatui::style::Modifier::BOLD),
                ),
                Span::raw(" text"),
            ]),
            Line::from(""),
            plain("tail"),
        ];
        let wrapped = wrap_lines(&lines, 6);
        assert_eq!(wrapped.len(), 4);
        assert_eq!(line_text(&wrapped[0]), "bold");
        assert_eq!(line_text(&wrapped[1]), "text");
        assert_eq!(line_text(&wrapped[2]), "");
        assert_eq!(line_text(&wrapped[3]), "tail");
        assert!(
            wrapped[0].spans[0]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
    }

    #[test]
    fn splits_words_that_are_wider_than_the_pane() {
        let lines = vec![plain("supercalifragilistic")];
        let wrapped = wrap_lines(&lines, 8);
        assert_eq!(
            wrapped.iter().map(line_text).collect::<Vec<_>>(),
            vec!["supercal", "ifragili", "stic"]
        );
    }
}
