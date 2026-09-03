//! Paragraph, inline text formatting (bold/italic/code), and horizontal rules.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub const INLINE_CODE_BG: Color = Color::Rgb(40, 44, 52);
pub const RULE_FG: Color = Color::Rgb(82, 88, 99);
const STRING_FG: Color = Color::Rgb(152, 195, 121);
const PLAIN_FG: Color = Color::White;

pub fn is_horizontal_rule(line: &str) -> bool {
    let leading_spaces = line.chars().take_while(|ch| *ch == ' ').count();
    if leading_spaces > 3 {
        return false;
    }
    let mut markers = line.trim().chars().filter(|ch| !ch.is_whitespace());
    let Some(marker) = markers.next() else {
        return false;
    };
    if !matches!(marker, '-' | '*' | '_') {
        return false;
    }
    let mut count = 1;
    for ch in markers {
        if ch != marker {
            return false;
        }
        count += 1;
    }
    count >= 3
}

pub fn render_paragraph(lines: &[String], width: usize) -> Vec<Line<'static>> {
    let joined = lines.join(" ");
    wrap_styled_line(render_inline(&joined), width)
}

pub fn wrap_styled_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = Vec::new();
    let mut used = 0;
    for span in line.spans {
        for grapheme in span.content.graphemes(true) {
            let grapheme_width = UnicodeWidthStr::width(grapheme);
            if used + grapheme_width > width && !current.is_empty() {
                lines.push(Line::from(std::mem::take(&mut current)));
                used = 0;
            }
            push_merged_span(&mut current, grapheme, span.style);
            used += grapheme_width;
        }
    }
    if !current.is_empty() {
        lines.push(Line::from(current));
    }
    if lines.is_empty() {
        lines.push(Line::raw(String::new()));
    }
    lines
}

pub fn render_inline(text: &str) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let plain_style = Style::default().fg(PLAIN_FG);
    parse_inline(text, plain_style, &mut spans);
    Line::from(spans)
}

pub fn parse_inline(text: &str, base_style: Style, spans: &mut Vec<Span<'static>>) {
    let code_style = Style::default()
        .fg(STRING_FG)
        .bg(INLINE_CODE_BG)
        .add_modifier(Modifier::BOLD);
    let mut idx = 0;
    while idx < text.len() {
        let rest = &text[idx..];

        if let Some(escaped) = rest.strip_prefix('\\') {
            if let Some(ch) = escaped.chars().next().filter(|ch| "\\`*_".contains(*ch)) {
                push_merged_span(spans, &ch.to_string(), base_style);
                idx += '\\'.len_utf8() + ch.len_utf8();
                continue;
            }
        }

        if let Some(after) = rest.strip_prefix('`') {
            if let Some(end) = after.find('`') {
                push_merged_span(spans, &after[..end], code_style);
                idx += 1 + end + 1;
                continue;
            }
        }

        let mut matched = false;
        for (delimiter, modifier) in [
            ("***", Modifier::BOLD | Modifier::ITALIC),
            ("___", Modifier::BOLD | Modifier::ITALIC),
            ("**", Modifier::BOLD),
            ("__", Modifier::BOLD),
            ("*", Modifier::ITALIC),
            ("_", Modifier::ITALIC),
        ] {
            let Some(after) = rest.strip_prefix(delimiter) else {
                continue;
            };
            if delimiter.starts_with('_')
                && idx > 0
                && text[..idx]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_alphanumeric)
            {
                continue;
            }
            let Some(end) = after.find(delimiter) else {
                continue;
            };
            if end == 0 {
                continue;
            }
            let following = &after[end + delimiter.len()..];
            if delimiter.starts_with('_')
                && following.chars().next().is_some_and(char::is_alphanumeric)
            {
                continue;
            }
            parse_inline(&after[..end], base_style.add_modifier(modifier), spans);
            idx += delimiter.len() + end + delimiter.len();
            matched = true;
            break;
        }
        if matched {
            continue;
        }

        let ch = rest.chars().next().unwrap();
        push_merged_span(spans, &ch.to_string(), base_style);
        idx += ch.len_utf8();
    }
}

fn push_merged_span(spans: &mut Vec<Span<'static>>, text: &str, style: Style) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut() {
        if last.style == style {
            last.content.to_mut().push_str(text);
            return;
        }
    }
    spans.push(Span::styled(text.to_owned(), style));
}
