//! Markdown GFM pipe table parsing and rendering with exact border alignment.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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

pub fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn longest_word_width(text: &str, max_width: usize) -> usize {
    let mut longest = 0;
    for word in text.split_whitespace() {
        longest = longest.max(display_width(word));
    }
    longest.min(max_width)
}

pub fn wrap_cell_text(text: &str, max_width: usize) -> Vec<String> {
    let max_w = max_width.max(1);
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_w = 0;

    for word in text.split_whitespace() {
        let word_w = display_width(word);
        if current.is_empty() {
            if word_w <= max_w {
                current.push_str(word);
                current_w = word_w;
            } else {
                // Word is wider than max_w; split by character
                for ch in word.chars() {
                    let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                    if current_w + cw > max_w && !current.is_empty() {
                        lines.push(current);
                        current = String::new();
                        current_w = 0;
                    }
                    current.push(ch);
                    current_w += cw;
                }
            }
        } else if current_w + 1 + word_w <= max_w {
            current.push(' ');
            current.push_str(word);
            current_w += 1 + word_w;
        } else {
            lines.push(current);
            current = String::new();
            current_w = 0;
            if word_w <= max_w {
                current.push_str(word);
                current_w = word_w;
            } else {
                for ch in word.chars() {
                    let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                    if current_w + cw > max_w && !current.is_empty() {
                        lines.push(current);
                        current = String::new();
                        current_w = 0;
                    }
                    current.push(ch);
                    current_w += cw;
                }
            }
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
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

    // Border overhead: "│ " + (cols - 1) * " │ " + " │" = 3 * cols + 1
    let border_overhead = 3 * cols + 1;
    if width < border_overhead + cols {
        // Too narrow to render table cleanly; return raw rows
        return rows
            .iter()
            .map(|r| Line::raw(format!("| {} |", r.join(" | "))))
            .collect();
    }

    let available_for_cells = width.saturating_sub(border_overhead);
    let max_unbroken = 30;

    // Calculate natural and minimum word widths
    let mut natural_widths = vec![1usize; cols];
    let mut min_word_widths = vec![1usize; cols];

    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < cols {
                natural_widths[i] = natural_widths[i].max(display_width(cell));
                min_word_widths[i] =
                    min_word_widths[i].max(longest_word_width(cell, max_unbroken).max(1));
            }
        }
    }

    let mut min_column_widths = min_word_widths.clone();
    let min_cells_width: usize = min_column_widths.iter().sum();

    if min_cells_width > available_for_cells {
        min_column_widths = vec![1; cols];
        let remaining = available_for_cells.saturating_sub(cols);
        if remaining > 0 {
            let total_weight: usize = min_word_widths.iter().map(|w| w.saturating_sub(1)).sum();
            let mut allocated = 0;
            let mut growths = vec![0; cols];
            for i in 0..cols {
                let weight = min_word_widths[i].saturating_sub(1);
                growths[i] = (weight * remaining).checked_div(total_weight).unwrap_or(0);
                min_column_widths[i] += growths[i];
                allocated += growths[i];
            }
            let mut leftover = remaining.saturating_sub(allocated);
            let mut idx = 0;
            while leftover > 0 && idx < cols {
                min_column_widths[idx] += 1;
                leftover -= 1;
                idx += 1;
            }
        }
    }

    let total_natural_width: usize = natural_widths.iter().sum::<usize>() + border_overhead;
    let mut column_widths = vec![1usize; cols];

    if total_natural_width <= width {
        for i in 0..cols {
            column_widths[i] = natural_widths[i].max(min_column_widths[i]);
        }
    } else {
        let cur_min_sum: usize = min_column_widths.iter().sum();
        let extra_width = available_for_cells.saturating_sub(cur_min_sum);
        let total_grow_potential: usize = (0..cols)
            .map(|i| natural_widths[i].saturating_sub(min_column_widths[i]))
            .sum();

        for i in 0..cols {
            let delta = natural_widths[i].saturating_sub(min_column_widths[i]);
            let grow = (delta * extra_width)
                .checked_div(total_grow_potential)
                .unwrap_or(0);
            column_widths[i] = min_column_widths[i] + grow;
        }

        let allocated: usize = column_widths.iter().sum();
        let mut remaining = available_for_cells.saturating_sub(allocated);
        while remaining > 0 {
            let mut grew = false;
            for i in 0..cols {
                if remaining == 0 {
                    break;
                }
                if column_widths[i] < natural_widths[i] {
                    column_widths[i] += 1;
                    remaining -= 1;
                    grew = true;
                }
            }
            if !grew {
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
    out.push(table_top_border(&column_widths, border_style));

    // Render header with wrapping
    let header = &rows[0];
    let header_cells: Vec<Vec<String>> = (0..cols)
        .map(|i| {
            let text = header.get(i).map(String::as_str).unwrap_or("");
            wrap_cell_text(text, column_widths[i])
        })
        .collect();
    let header_lines_count = header_cells.iter().map(|c| c.len()).max().unwrap_or(1);

    for line_idx in 0..header_lines_count {
        let mut spans = Vec::new();
        spans.push(Span::styled("│", border_style));
        for i in 0..cols {
            let text = header_cells[i]
                .get(line_idx)
                .map(String::as_str)
                .unwrap_or("");
            let align = aligns.get(i).copied().unwrap_or(Alignment::Left);
            let padded = pad_cell(text, column_widths[i], align);
            spans.push(Span::styled(padded, header_style));
            spans.push(Span::styled("│", border_style));
        }
        out.push(Line::from(spans));
    }

    // Separator line after header
    let sep = separator_line(&column_widths, border_style, separator_style);
    out.push(sep.clone());

    // Render data rows with wrapping and separators (Pi Agent style)
    for (row_idx, row) in rows[1..].iter().enumerate() {
        let row_cells: Vec<Vec<String>> = (0..cols)
            .map(|i| {
                let text = row.get(i).map(String::as_str).unwrap_or("");
                wrap_cell_text(text, column_widths[i])
            })
            .collect();
        let row_lines_count = row_cells.iter().map(|c| c.len()).max().unwrap_or(1);

        for line_idx in 0..row_lines_count {
            let mut spans = Vec::new();
            spans.push(Span::styled("│", border_style));
            for i in 0..cols {
                let text = row_cells[i].get(line_idx).map(String::as_str).unwrap_or("");
                let align = aligns.get(i).copied().unwrap_or(Alignment::Left);
                let padded = pad_cell(text, column_widths[i], align);
                spans.push(Span::styled(padded, cell_style));
                spans.push(Span::styled("│", border_style));
            }
            out.push(Line::from(spans));
        }

        if row_idx + 1 < rows.len() - 1 {
            out.push(sep.clone());
        }
    }

    // Bottom border
    out.push(table_bottom_border(&column_widths, border_style));
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
    let display = display_width(text);
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
    format!(" {}{}{} ", " ".repeat(left), text, " ".repeat(right))
}
