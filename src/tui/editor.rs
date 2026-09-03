//! Unicode-aware text editing, cursor geometry, and wrapping utilities.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub fn editor_rows(text: &str, width: usize) -> usize {
    cursor_position(text, text.len(), width).1 + 1
}

pub fn cursor_position(text: &str, cursor: usize, width: usize) -> (usize, usize) {
    let width = width.max(1);
    let mut column = 0usize;
    let mut row = 0usize;
    for grapheme in text[..cursor].graphemes(true) {
        if grapheme == "\n" {
            row += 1;
            column = 0;
            continue;
        }
        let size = UnicodeWidthStr::width(grapheme);
        if column + size > width {
            row += 1;
            column = 0;
        }
        column += size;
        if column == width {
            row += 1;
            column = 0;
        }
    }
    (column, row)
}

pub fn previous_grapheme_boundary(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .grapheme_indices(true)
        .next_back()
        .map(|(index, _)| index)
        .unwrap_or(0)
}

pub fn next_grapheme_boundary(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .graphemes(true)
        .next()
        .map(|value| cursor + value.len())
        .unwrap_or(cursor)
}

pub fn line_start(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0)
}

pub fn line_end(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .find('\n')
        .map(|index| cursor + index)
        .unwrap_or(text.len())
}

pub fn truncate_to_width(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width {
        return text.to_owned();
    }
    if width <= 1 {
        return "…".chars().take(width).collect();
    }
    let mut result = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let size = UnicodeWidthStr::width(grapheme);
        if used + size > width - 1 {
            break;
        }
        result.push_str(grapheme);
        used += size;
    }
    result.push('…');
    result
}

pub fn join_sides(left: &str, right: &str, width: usize) -> String {
    let right = truncate_to_width(right, width / 2);
    let right_width = UnicodeWidthStr::width(right.as_str());
    let left = truncate_to_width(left, width.saturating_sub(right_width + 2));
    let padding = width.saturating_sub(UnicodeWidthStr::width(left.as_str()) + right_width);
    format!("{left}{}{right}", " ".repeat(padding))
}

pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let size = UnicodeWidthStr::width(grapheme);
        if used + size > width && !current.is_empty() {
            out.push(std::mem::take(&mut current));
            used = 0;
        }
        current.push_str(grapheme);
        used += size;
    }
    out.push(current);
    out
}
