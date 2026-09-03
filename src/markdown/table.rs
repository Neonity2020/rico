//! Markdown GFM pipe table parsing and rendering with exact border alignment.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

const TABLE_HEADER_BG: Color = Color::Rgb(50, 54, 62);
const PLAIN_FG: Color = Color::White;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    Left,
    Right,
    Center,
}

pub fn is_table_row(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || !trimmed.contains('|') {
        return false;
    }
    let trimmed = trimmed.trim_matches(|c| c == ' ' || c == '|');
    !trimmed.is_empty()
}

pub fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim().trim_matches(|c| c == ' ' || c == '|');
    if trimmed.is_empty() {
        return false;
    }
    trimmed.split('|').all(|cell| {
        let cell = cell.trim();
        !cell.is_empty()
            && cell
                .chars()
                .all(|ch| ch == '-' || ch == ':' || ch.is_whitespace())
    })
}

pub fn parse_row(line: &str) -> Vec<String> {
    let trimmed = line.trim().trim_matches(|c| c == '|');
    // Preserve whitespace inside cells by trimming only the edges.
    trimmed
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

pub fn parse_alignments(line: &str) -> Vec<Alignment> {
    let trimmed = line.trim().trim_matches(|c| c == '|');
    trimmed
        .split('|')
        .map(|cell| {
            let cell = cell.trim();
            let left = cell.starts_with(':');
            let right = cell.ends_with(':');
            match (left, right) {
                (true, true) => Alignment::Center,
                (false, true) => Alignment::Right,
                _ => Alignment::Left,
            }
        })
        .collect()
}

pub fn render_table(
    rows: &[Vec<String>],
    aligns: &[Alignment],
    width: usize,
) -> Vec<Line<'static>> {
    if rows.is_empty() {
        return Vec::new();
    }
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if cols == 0 {
        return Vec::new();
    }
    let mut widths = vec![3usize; cols];
    for row in rows {
        for (idx, cell) in row.iter().enumerate() {
            widths[idx] = widths[idx].max(display_width(cell).max(3));
        }
    }
    // Total width: 1 (leading "│") + sum(w + 2 for cell padding + 1 for border)
    let total: usize = 1 + widths.iter().map(|w| w + 3).sum::<usize>();
    if total > width {
        // Shrink widest columns evenly.
        let mut guard = 0usize;
        while 1 + widths.iter().map(|w| w + 3).sum::<usize>() > width {
            let (idx, _) = widths.iter().enumerate().max_by_key(|(_, w)| **w).unwrap();
            if widths[idx] <= 3 {
                break;
            }
            widths[idx] -= 1;
            guard += 1;
            if guard > 10_000 {
                break;
            }
        }
    }

    let border_style = Style::default().fg(Color::DarkGray);
    let header_style = Style::default()
        .fg(Color::White)
        .bg(TABLE_HEADER_BG)
        .add_modifier(Modifier::BOLD);
    let cell_style = Style::default().fg(PLAIN_FG);
    let separator_style = Style::default().fg(Color::DarkGray);

    let mut out = Vec::new();
    // Top border
    out.push(table_top_border(&widths, border_style));

    let header = &rows[0];
    out.push(table_line(
        header,
        aligns,
        &widths,
        border_style,
        header_style,
        true,
    ));
    out.push(separator_line(&widths, border_style, separator_style));
    for row in &rows[1..] {
        out.push(table_line(
            row,
            aligns,
            &widths,
            border_style,
            cell_style,
            false,
        ));
    }
    // Bottom border
    out.push(table_bottom_border(&widths, border_style));
    out
}

fn table_top_border(widths: &[usize], border_style: Style) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::styled("┌", border_style));
    for (idx, w) in widths.iter().enumerate() {
        spans.push(Span::styled("─".repeat(w + 2), border_style));
        if idx + 1 < widths.len() {
            spans.push(Span::styled("┬", border_style));
        }
    }
    spans.push(Span::styled("┐", border_style));
    Line::from(spans)
}

fn table_bottom_border(widths: &[usize], border_style: Style) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::styled("└", border_style));
    for (idx, w) in widths.iter().enumerate() {
        spans.push(Span::styled("─".repeat(w + 2), border_style));
        if idx + 1 < widths.len() {
            spans.push(Span::styled("┴", border_style));
        }
    }
    spans.push(Span::styled("┘", border_style));
    Line::from(spans)
}

fn table_line(
    row: &[String],
    aligns: &[Alignment],
    widths: &[usize],
    border_style: Style,
    cell_style: Style,
    is_header: bool,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::styled("│", border_style));
    for (idx, _) in widths.iter().enumerate() {
        let cell = row.get(idx).cloned().unwrap_or_default();
        let align = aligns.get(idx).copied().unwrap_or(Alignment::Left);
        let padded = pad_cell(&cell, widths[idx], align);
        let style = if is_header {
            header_inner_style(cell_style)
        } else {
            cell_style
        };
        spans.push(Span::styled(padded, style));
        spans.push(Span::styled("│", border_style));
    }
    Line::from(spans)
}

fn header_inner_style(base: Style) -> Style {
    base.bg(TABLE_HEADER_BG).add_modifier(Modifier::BOLD)
}

fn separator_line(widths: &[usize], border_style: Style, line_style: Style) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::styled("├", border_style));
    for (idx, w) in widths.iter().enumerate() {
        spans.push(Span::styled("─".repeat(w + 2), line_style));
        if idx + 1 < widths.len() {
            spans.push(Span::styled("┼", border_style));
        }
    }
    spans.push(Span::styled("┤", border_style));
    Line::from(spans)
}

pub fn pad_cell(text: &str, width: usize, align: Alignment) -> String {
    let truncated = if display_width(text) > width {
        truncate(text, width)
    } else {
        text.to_string()
    };
    let display = display_width(&truncated);
    let pad = width.saturating_sub(display);
    let (left, right) = match align {
        Alignment::Left => (0, pad),
        Alignment::Right => (pad, 0),
        Alignment::Center => {
            let l = pad / 2;
            let r = pad - l;
            (l, r)
        }
    };
    format!(" {}{}{} ", " ".repeat(left), truncated, " ".repeat(right))
}

pub fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let dw = display_width(text);
    if dw <= width {
        return text.to_string();
    }
    if width == 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    let limit = width - 1; // reserve 1 cell for '…'
    for ch in text.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + cw > limit {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    used += 1;
    if used < width {
        out.push_str(&" ".repeat(width - used));
    }
    out
}

pub fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}
