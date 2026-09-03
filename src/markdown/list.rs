//! Markdown list rendering: unordered bullets, ordered numbers, task checkboxes, and hanging indents.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::inline::{render_inline, wrap_styled_line};

pub const BULLET_COLOR: Color = Color::Cyan;
pub const TASK_DONE_COLOR: Color = Color::Green;
pub const TASK_TODO_COLOR: Color = Color::DarkGray;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListKind {
    Unordered,
    Ordered(usize),
    Task(bool),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItem {
    pub indent_spaces: usize,
    pub kind: ListKind,
    pub content: String,
}

pub fn parse_list_item(line: &str) -> Option<ListItem> {
    let trimmed_start = line.trim_start();
    if trimmed_start.is_empty() {
        return None;
    }
    let indent_spaces = line.len() - trimmed_start.len();

    // Task list items: - [ ] or - [x]
    if let Some(rest) = trimmed_start
        .strip_prefix("- [ ] ")
        .or_else(|| trimmed_start.strip_prefix("* [ ] "))
        .or_else(|| trimmed_start.strip_prefix("+ [ ] "))
    {
        return Some(ListItem {
            indent_spaces,
            kind: ListKind::Task(false),
            content: rest.to_string(),
        });
    }

    if let Some(rest) = trimmed_start
        .strip_prefix("- [x] ")
        .or_else(|| trimmed_start.strip_prefix("- [X] "))
        .or_else(|| trimmed_start.strip_prefix("* [x] "))
        .or_else(|| trimmed_start.strip_prefix("* [X] "))
        .or_else(|| trimmed_start.strip_prefix("+ [x] "))
        .or_else(|| trimmed_start.strip_prefix("+ [X] "))
    {
        return Some(ListItem {
            indent_spaces,
            kind: ListKind::Task(true),
            content: rest.to_string(),
        });
    }

    // Unordered bullets: - , * , +
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed_start.strip_prefix(marker) {
            return Some(ListItem {
                indent_spaces,
                kind: ListKind::Unordered,
                content: rest.to_string(),
            });
        }
    }

    // Ordered numbers: 1. , 2. etc.
    let digits_count = trimmed_start
        .chars()
        .take_while(char::is_ascii_digit)
        .count();
    if digits_count > 0 && digits_count <= 9 {
        let after_digits = &trimmed_start[digits_count..];
        if let Some(rest) = after_digits.strip_prefix(". ") {
            if let Ok(num) = trimmed_start[..digits_count].parse::<usize>() {
                return Some(ListItem {
                    indent_spaces,
                    kind: ListKind::Ordered(num),
                    content: rest.to_string(),
                });
            }
        }
    }

    None
}

pub fn render_list_item(item: &ListItem, width: usize) -> Vec<Line<'static>> {
    let level = item.indent_spaces / 2;
    let base_indent = "  ".repeat(level);
    let base_indent_width = level * 2;

    let (bullet_str, bullet_style) = match item.kind {
        ListKind::Unordered => {
            let symbol = match level {
                0 => "• ",
                1 => "◦ ",
                _ => "▪ ",
            };
            (
                symbol.to_string(),
                Style::default()
                    .fg(BULLET_COLOR)
                    .add_modifier(Modifier::BOLD),
            )
        }
        ListKind::Ordered(num) => (
            format!("{num}. "),
            Style::default()
                .fg(BULLET_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        ListKind::Task(true) => (
            "☑ ".to_string(),
            Style::default()
                .fg(TASK_DONE_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        ListKind::Task(false) => ("☐ ".to_string(), Style::default().fg(TASK_TODO_COLOR)),
    };

    let bullet_width = UnicodeWidthStr::width(bullet_str.as_str());
    let prefix_width = base_indent_width + bullet_width;
    let available_content_width = width.saturating_sub(prefix_width).max(10);

    let content_line = render_inline(&item.content);
    let wrapped_content = wrap_styled_line(content_line, available_content_width);

    let mut out = Vec::new();
    let hanging_indent_spaces = " ".repeat(prefix_width);

    for (idx, line) in wrapped_content.into_iter().enumerate() {
        if idx == 0 {
            let mut spans = Vec::new();
            if !base_indent.is_empty() {
                spans.push(Span::raw(base_indent.clone()));
            }
            spans.push(Span::styled(bullet_str.clone(), bullet_style));
            spans.extend(line.spans);
            out.push(Line::from(spans));
        } else {
            let mut spans = Vec::new();
            spans.push(Span::raw(hanging_indent_spaces.clone()));
            spans.extend(line.spans);
            out.push(Line::from(spans));
        }
    }

    out
}

pub fn render_list(lines: &[String], width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut current_item: Option<ListItem> = None;

    for line in lines {
        if let Some(item) = parse_list_item(line) {
            if let Some(prev) = current_item.take() {
                out.extend(render_list_item(&prev, width));
            }
            current_item = Some(item);
        } else if let Some(ref mut item) = current_item {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                item.content.push(' ');
                item.content.push_str(trimmed);
            }
        }
    }

    if let Some(last) = current_item {
        out.extend(render_list_item(&last, width));
    }

    out
}
