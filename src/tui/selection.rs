//! Mouse transcript selection, highlight rendering, and clipboard integration.

use std::{
    io::{stdout, Write},
    process::{Command, Stdio},
};

use base64::Engine;
use ratatui::style::Modifier;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::app::{App, SelectionPoint};

pub fn transcript_point(app: &App, column: u16, row: u16) -> Option<SelectionPoint> {
    let area = app.transcript_area;
    if column < area.x || column >= area.right() || row < area.y || row >= area.bottom() {
        return None;
    }
    transcript_point_clamped(app, column, row)
}

pub fn transcript_point_clamped(app: &App, column: u16, row: u16) -> Option<SelectionPoint> {
    let area = app.transcript_area;
    if area.is_empty() || app.transcript_lines.is_empty() {
        return None;
    }
    let screen_row = row.clamp(area.y, area.bottom().saturating_sub(1));
    let logical_row = (app.transcript_scroll.top + usize::from(screen_row - area.y))
        .min(app.transcript_lines.len() - 1);
    let line_width = UnicodeWidthStr::width(app.transcript_lines[logical_row].as_str());
    let screen_column = column.clamp(area.x, area.right().saturating_sub(1));
    Some(SelectionPoint {
        row: logical_row,
        column: usize::from(screen_column - area.x).min(line_width),
    })
}

pub fn selection_bounds(app: &App) -> Option<(SelectionPoint, SelectionPoint)> {
    let anchor = app.selection_anchor?;
    let focus = app.selection_focus?;
    if anchor == focus {
        return None;
    }
    if (anchor.row, anchor.column) <= (focus.row, focus.column) {
        Some((anchor, focus))
    } else {
        Some((focus, anchor))
    }
}

pub fn grapheme_cell_range(text: &str, column: usize) -> Option<(usize, usize)> {
    let mut cell = 0;
    for grapheme in text.graphemes(true) {
        let width = UnicodeWidthStr::width(grapheme);
        let end = cell + width;
        if column < end || (width == 0 && column == cell) {
            return Some((cell, end.max(cell + 1)));
        }
        cell = end;
    }
    None
}

pub fn selection_columns(
    line: &str,
    row: usize,
    start: SelectionPoint,
    end: SelectionPoint,
) -> (usize, usize) {
    let width = UnicodeWidthStr::width(line);
    let from = if row == start.row {
        grapheme_cell_range(line, start.column)
            .map(|range| range.0)
            .unwrap_or(start.column.min(width))
    } else {
        0
    };
    let to = if row == end.row {
        grapheme_cell_range(line, end.column)
            .map(|range| range.1)
            .unwrap_or(end.column.min(width))
    } else {
        width
    };
    (from.min(width), to.min(width))
}

pub fn render_selection(frame: &mut ratatui::Frame<'_>, app: &App) {
    let Some((start, end)) = selection_bounds(app) else {
        return;
    };
    let area = app.transcript_area;
    if area.is_empty() {
        return;
    }
    let first_visible = app.transcript_scroll.top;
    let last_visible = first_visible.saturating_add(area.height as usize);
    for row in start.row.max(first_visible)..=end.row.min(last_visible.saturating_sub(1)) {
        let Some(line) = app.transcript_lines.get(row) else {
            continue;
        };
        let (from, to) = selection_columns(line, row, start, end);
        let y = area.y + (row - first_visible) as u16;
        for column in from..to.min(area.width as usize) {
            if let Some(cell) = frame.buffer_mut().cell_mut((area.x + column as u16, y)) {
                cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
            }
        }
    }
}

pub fn selected_text(app: &App) -> Option<String> {
    let (start, end) = selection_bounds(app)?;
    let mut selected = Vec::new();
    for row in start.row..=end.row {
        let line = app.transcript_lines.get(row)?;
        let (from, to) = selection_columns(line, row, start, end);
        selected.push(slice_by_columns(line, from, to).trim_end().to_owned());
    }
    let text = selected.join("\n");
    (!text.is_empty()).then_some(text)
}

pub fn slice_by_columns(text: &str, from: usize, to: usize) -> String {
    let mut result = String::new();
    let mut cell = 0;
    for grapheme in text.graphemes(true) {
        let end = cell + UnicodeWidthStr::width(grapheme);
        if end > from && cell < to {
            result.push_str(grapheme);
        }
        cell = end;
    }
    result
}

pub fn copy_to_clipboard(text: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        if let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
            if child
                .stdin
                .take()
                .is_some_and(|mut input| input.write_all(text.as_bytes()).is_ok())
                && child.wait().is_ok_and(|status| status.success())
            {
                return true;
            }
        }
    }

    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let mut terminal = stdout();
    write!(terminal, "\x1b]52;c;{encoded}\x07").is_ok() && terminal.flush().is_ok()
}
