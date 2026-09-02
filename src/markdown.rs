//! Lightweight Markdown rendering for the assistant pane.
//!
//! Supports the constructs the coding agent actually emits:
//! - Fenced code blocks with optional language tags, rendered with simple
//!   syntax-aware styling (comments / strings / numbers / keywords).
//! - Pipe tables rendered as aligned columns with a header row.
//! - Plain paragraphs with `inline code` styling and `-` / `*` bullet lists.
//!
//! Output is a sequence of ratatui [`Line`]s that owns its spans, so it can be
//! combined freely with the rest of the TUI styling.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const INLINE_CODE_BG: Color = Color::Rgb(40, 44, 52);
const TABLE_HEADER_BG: Color = Color::Rgb(50, 54, 62);
const RULE_FG: Color = Color::Rgb(82, 88, 99);

const COMMENT_FG: Color = Color::Rgb(127, 139, 154);
const STRING_FG: Color = Color::Rgb(152, 195, 121);
const NUMBER_FG: Color = Color::Rgb(209, 154, 102);
const KEYWORD_FG: Color = Color::Rgb(198, 120, 221);
const FUNCTION_FG: Color = Color::Rgb(97, 175, 239);
const TYPE_FG: Color = Color::Rgb(229, 192, 123);
const PLAIN_FG: Color = Color::White;

const KEYWORDS_COMMON: &[&str] = &[
    // Rust
    "fn",
    "let",
    "mut",
    "const",
    "static",
    "if",
    "else",
    "match",
    "for",
    "while",
    "loop",
    "return",
    "break",
    "continue",
    "struct",
    "enum",
    "trait",
    "impl",
    "pub",
    "use",
    "mod",
    "async",
    "await",
    "move",
    "ref",
    "where",
    "dyn",
    "as",
    "in",
    "self",
    "Self",
    // Python
    "def",
    "class",
    "import",
    "from",
    "try",
    "except",
    "finally",
    "with",
    "yield",
    "lambda",
    "raise",
    "pass",
    "global",
    "nonlocal",
    "None",
    "True",
    "False",
    // JavaScript / TypeScript
    "function",
    "var",
    "let",
    "this",
    "new",
    "typeof",
    "instanceof",
    "class",
    "extends",
    "export",
    "import",
    "default",
    "switch",
    "case",
    "do",
    // C / C++ / Go / Java
    "int",
    "char",
    "float",
    "double",
    "void",
    "unsigned",
    "signed",
    "short",
    "long",
    "goto",
    "sizeof",
    "typedef",
    "struct",
    "union",
    "enum",
    "volatile",
    "register",
    "package",
    "interface",
    "throws",
    "throw",
    "new",
    "null",
    "true",
    "false",
    // Shell / config
    "if",
    "then",
    "else",
    "elif",
    "fi",
    "for",
    "while",
    "do",
    "done",
    "case",
    "esac",
    "function",
    "return",
    "in",
];

const TYPES_COMMON: &[&str] = &[
    "bool", "byte", "str", "String", "Option", "Result", "Vec", "Box", "Rc", "Arc", "HashMap",
    "HashSet", "BTreeMap", "BTreeSet", "int", "i8", "i16", "i32", "i64", "i128", "isize", "uint",
    "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64", "usize", "list", "dict", "set",
    "tuple", "object", "number", "string", "boolean", "object", "Any", "Self",
];

/// Render a Markdown body into `Vec<Line<'static>>` ready for the TUI.
///
/// `width` is the available text width (in cells) — used for wrapping
/// paragraphs and computing table column widths.
pub fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![Line::raw(String::new())];
    }
    let mut out: Vec<Line<'static>> = Vec::new();
    let lines = text.split('\n').map(|s| s.to_string()).collect::<Vec<_>>();
    let mut i = 0;
    while i < lines.len() {
        let raw = lines[i].as_str();
        let stripped = raw.trim();
        if let Some(rest) = stripped.strip_prefix("```") {
            let lang = rest.trim().to_lowercase();
            let lang_tag = if lang.is_empty() { None } else { Some(lang) };
            i += 1;
            let mut code = String::new();
            let start = i;
            while i < lines.len() {
                if lines[i].trim_start().starts_with("```") {
                    break;
                }
                i += 1;
            }
            for line in &lines[start..i] {
                code.push_str(line);
                code.push('\n');
            }
            if i < lines.len() {
                i += 1;
            }
            out.push(Line::raw(String::new()));
            out.extend(render_code_block(&code, lang_tag.as_deref(), width));
            out.push(Line::raw(String::new()));
            continue;
        }

        // Collect a table: header line + separator + body rows.
        if is_table_row(raw) && i + 1 < lines.len() && is_table_separator(lines[i + 1].as_str()) {
            let header = parse_row(raw);
            i += 1; // skip separator
            let aligns = parse_alignments(&lines[i - 1]);
            let mut rows: Vec<Vec<String>> = vec![header];
            while i < lines.len() && is_table_row(lines[i].as_str()) {
                rows.push(parse_row(&lines[i]));
                i += 1;
            }
            out.push(Line::raw(String::new()));
            out.extend(render_table(&rows, &aligns, width));
            out.push(Line::raw(String::new()));
            continue;
        }

        if is_horizontal_rule(raw) {
            out.push(Line::from(Span::styled(
                "─".repeat(width),
                Style::default().fg(RULE_FG),
            )));
            i += 1;
            continue;
        }

        // Plain paragraph(s). Collect contiguous non-empty lines into one block.
        let mut paragraph = Vec::new();
        while i < lines.len() {
            let current = lines[i].as_str();
            if current.trim().is_empty() {
                break;
            }
            if current.trim_start().starts_with("```") {
                break;
            }
            if is_horizontal_rule(current) {
                break;
            }
            if is_table_row(current)
                && i + 1 < lines.len()
                && is_table_separator(lines[i + 1].as_str())
            {
                break;
            }
            paragraph.push(current.to_string());
            i += 1;
        }
        if !paragraph.is_empty() {
            out.extend(render_paragraph(&paragraph, width));
        }
        // Blank line separates blocks.
        if i < lines.len() && lines[i].trim().is_empty() {
            i += 1;
        }
    }
    out
}

fn is_horizontal_rule(line: &str) -> bool {
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

// We don't actually need fence preservation beyond split('\n'); the
// variable is inlined to keep the contract explicit.

fn is_table_row(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || !trimmed.contains('|') {
        return false;
    }
    let trimmed = trimmed.trim_matches(|c| c == ' ' || c == '|');
    !trimmed.is_empty()
}

fn is_table_separator(line: &str) -> bool {
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

fn parse_row(line: &str) -> Vec<String> {
    let trimmed = line.trim().trim_matches(|c| c == '|');
    // Preserve whitespace inside cells by trimming only the edges.
    trimmed
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

#[derive(Debug, Clone, Copy)]
enum Alignment {
    Left,
    Right,
    Center,
}

fn parse_alignments(line: &str) -> Vec<Alignment> {
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

fn render_table(rows: &[Vec<String>], aligns: &[Alignment], width: usize) -> Vec<Line<'static>> {
    if rows.is_empty() {
        return Vec::new();
    }
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut widths = vec![3usize; cols];
    for row in rows {
        for (idx, cell) in row.iter().enumerate() {
            widths[idx] = widths[idx].max(display_width(cell).max(3));
        }
    }
    // Total width: 1 (leading "|") + cols * (width + 3)...
    let mut total: usize = 1;
    for w in &widths {
        total += w + 3;
    }
    if total > width {
        // Shrink widest columns evenly.
        let mut guard = 0usize;
        while widths.iter().sum::<usize>() + widths.len() * 3 + 1 > width {
            let (idx, _) = widths.iter().enumerate().max_by_key(|(_, w)| **w).unwrap();
            if widths[idx] <= 3 {
                break;
            }
            widths[idx] -= 1;
            if widths.iter().sum::<usize>() + widths.len() * 3 < width {
                break;
            }
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
    out
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
        let truncated = truncate(&cell, widths[idx]);
        let padded = pad_cell(&truncated, widths[idx], align);
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

fn pad_cell(text: &str, width: usize, align: Alignment) -> String {
    let display = display_width(text);
    if display >= width {
        return truncate(text, width);
    }
    let pad = width - display;
    let (left_pad, right_pad) = match align {
        Alignment::Left => (1, pad + 1),
        Alignment::Right => (pad + 1, 1),
        Alignment::Center => {
            let l = pad / 2 + 1;
            let r = pad - l + 2;
            (l, r)
        }
    };
    format!(
        " {}{}{} ",
        " ".repeat(left_pad - 1),
        text,
        " ".repeat(right_pad - 1)
    )
}

fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if display_width(text) <= width {
        return text.to_string();
    }
    if width == 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    let limit = width - 1; // reserve one cell for ellipsis
    for ch in text.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + cw > limit {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    out
}

fn display_width(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}

fn render_code_block(code: &str, lang: Option<&str>, width: usize) -> Vec<Line<'static>> {
    let tag = lang.unwrap_or("");
    let label = if tag.is_empty() {
        " 代码 ".to_string()
    } else {
        format!(" {tag} ")
    };
    let mut out = Vec::new();
    let mut buffer: Vec<Line<'static>> = Vec::new();
    // Push the highlighted lines first, then trim/compute pad based on longest.
    let mut longest = label.chars().count();
    let highlighted: Vec<Vec<Span<'static>>> = code
        .split('\n')
        .map(|line| highlight_code(line, tag))
        .collect();
    for line in &highlighted {
        let w: usize = line.iter().map(|s| display_width(s.content.as_ref())).sum();
        longest = longest.max(w + 4);
    }
    longest = longest.min(width);

    // Top border.
    out.push(frame_border(&label, '+', '-', longest, false));
    for line in highlighted {
        buffer.push(Line::from(line));
    }
    // Wrap long code lines to fit inside width.
    for line in buffer.into_iter() {
        let span_width: usize = line.iter().map(|s| display_width(s.content.as_ref())).sum();
        if span_width + 2 <= width {
            out.push(code_line(line, span_width));
        } else {
            out.extend(wrap_code_line(line, width));
        }
    }
    out.push(frame_border(&label, '+', '-', longest, true));
    out
}

fn frame_border(
    label: &str,
    corner: char,
    filler: char,
    width: usize,
    bottom: bool,
) -> Line<'static> {
    let label_width = display_width(label);
    let filler_str = filler.to_string();
    if width < label_width + 4 {
        return Line::from(Span::styled(
            format!(
                "{corner}{}{corner}",
                filler_str.repeat(width.saturating_sub(2))
            ),
            Style::default().fg(Color::DarkGray),
        ));
    }
    let left = width / 2 - label_width / 2;
    let right = width - left - label_width;
    let mut buf = String::new();
    buf.push(corner);
    buf.push_str(&filler_str.repeat(left.saturating_sub(1)));
    buf.push_str(label);
    buf.push_str(&filler_str.repeat(right.saturating_sub(1)));
    buf.push(if bottom { '┘' } else { '┐' });
    if bottom {
        buf = format!(
            "{}{}{}",
            corner,
            filler_str.repeat(width.saturating_sub(2)),
            corner
        );
    }
    Line::from(Span::styled(buf, Style::default().fg(Color::DarkGray)))
}

fn code_line(line: Line<'static>, used: usize) -> Line<'static> {
    let mut spans = Vec::with_capacity(line.spans.len() + 2);
    spans.push(Span::styled("│ ", Style::default().fg(Color::DarkGray)));
    spans.extend(line.spans);
    spans.push(Span::styled(" │", Style::default().fg(Color::DarkGray)));
    let _ = used;
    Line::from(spans)
}

fn wrap_code_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut current_width: usize = 0;
    let inner = width.saturating_sub(4);

    for span in line.spans.into_iter() {
        let style = span.style;
        let text = span.content.into_owned();
        // Split the span's text into graphemes, preserving style.
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let ch = chars[i];
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if current_width + cw > inner && !current_spans.is_empty() {
                let mut line_spans = Vec::with_capacity(current_spans.len() + 2);
                line_spans.push(Span::styled("│ ", Style::default().fg(Color::DarkGray)));
                line_spans.extend(std::mem::take(&mut current_spans));
                line_spans.push(Span::styled(" │", Style::default().fg(Color::DarkGray)));
                out.push(Line::from(line_spans));
                current_width = 0;
            }
            current_spans.push(Span::styled(ch.to_string(), style));
            current_width += cw;
            i += 1;
        }
    }
    if !current_spans.is_empty() || out.is_empty() {
        let mut line_spans = Vec::with_capacity(current_spans.len() + 2);
        line_spans.push(Span::styled("│ ", Style::default().fg(Color::DarkGray)));
        line_spans.extend(current_spans);
        line_spans.push(Span::styled(" │", Style::default().fg(Color::DarkGray)));
        out.push(Line::from(line_spans));
    }
    out
}

fn highlight_code(line: &str, lang: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let plain_style = Style::default().fg(PLAIN_FG);
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    if indent > 0 {
        spans.push(Span::styled(" ".repeat(indent), plain_style));
    }
    let trimmed_start = indent;
    let bytes = line.as_bytes();

    let mut idx = trimmed_start;
    while idx < bytes.len() {
        let rest = &line[idx..];
        let ch = rest.chars().next().unwrap();

        // Line comment: # for python/shell, % for yaml, // for C-family
        if (lang_is_shell_like(lang) || lang.is_empty() || lang == "py" || lang == "python")
            && ch == '#'
        {
            let end = line.len();
            spans.push(Span::styled(
                line[idx..end].to_string(),
                Style::default()
                    .fg(COMMENT_FG)
                    .add_modifier(Modifier::ITALIC),
            ));
            idx = end;
            continue;
        }
        if (lang_is_c_like(lang) || lang.is_empty()) && rest.starts_with("//") {
            let end = line.len();
            spans.push(Span::styled(
                line[idx..end].to_string(),
                Style::default()
                    .fg(COMMENT_FG)
                    .add_modifier(Modifier::ITALIC),
            ));
            idx = end;
            continue;
        }
        if (lang == "yaml" || lang == "yml") && ch == '#' {
            let end = line.len();
            spans.push(Span::styled(
                line[idx..end].to_string(),
                Style::default()
                    .fg(COMMENT_FG)
                    .add_modifier(Modifier::ITALIC),
            ));
            idx = end;
            continue;
        }

        // String literals
        if ch == '"' || ch == '\'' {
            let quote = ch;
            let mut end = idx + ch.len_utf8();
            while end < bytes.len() {
                let c = line[end..].chars().next().unwrap();
                if c == '\\' && end + c.len_utf8() < bytes.len() {
                    end += c.len_utf8()
                        + line[end + c.len_utf8()..]
                            .chars()
                            .next()
                            .unwrap()
                            .len_utf8();
                    continue;
                }
                if c == quote {
                    end += c.len_utf8();
                    break;
                }
                end += c.len_utf8();
            }
            spans.push(Span::styled(
                line[idx..end].to_string(),
                Style::default().fg(STRING_FG),
            ));
            idx = end;
            continue;
        }

        // Numbers
        if ch.is_ascii_digit() {
            let mut end = idx + ch.len_utf8();
            while end < bytes.len() {
                let c = line[end..].chars().next().unwrap();
                if c.is_ascii_alphanumeric()
                    || c == '.'
                    || c == '_'
                    || c == 'x'
                    || c == 'X'
                    || c == 'b'
                    || c == 'o'
                {
                    end += c.len_utf8();
                } else {
                    break;
                }
            }
            spans.push(Span::styled(
                line[idx..end].to_string(),
                Style::default().fg(NUMBER_FG),
            ));
            idx = end;
            continue;
        }

        // Identifier / keyword
        if is_ident_start(ch) {
            let mut end = idx + ch.len_utf8();
            while end < bytes.len() {
                let c = line[end..].chars().next().unwrap();
                if is_ident_cont(c) {
                    end += c.len_utf8();
                } else {
                    break;
                }
            }
            let word = &line[idx..end];
            let mut style = classify_word(word, lang);
            let followed_by_paren = end < bytes.len() && line[end..].starts_with('(');
            if followed_by_paren
                && !is_reserved_keyword(word)
                && looks_like_function_call_context(&line[..idx])
            {
                style = Style::default().fg(FUNCTION_FG);
            }
            spans.push(Span::styled(word.to_string(), style));
            if followed_by_paren {
                spans.push(Span::styled("(", plain_style));
                idx = end + 1;
                continue;
            } else {
                idx = end;
                continue;
            }
        }

        // Punctuation / operators
        spans.push(Span::styled(ch.to_string(), plain_style));
        idx += ch.len_utf8();
    }
    spans
}

fn classify_word(word: &str, lang: &str) -> Style {
    if KEYWORDS_COMMON.iter().any(|k| k.eq_ignore_ascii_case(word)) {
        return Style::default().fg(KEYWORD_FG).add_modifier(Modifier::BOLD);
    }
    if TYPES_COMMON.iter().any(|k| k.eq_ignore_ascii_case(word)) {
        return Style::default().fg(TYPE_FG);
    }
    // Heuristic: languages with type-after-name (TS/JS) — uppercase = type.
    if lang_is_c_like(lang) && word.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
        return Style::default().fg(TYPE_FG);
    }
    Style::default().fg(PLAIN_FG)
}

fn is_reserved_keyword(word: &str) -> bool {
    KEYWORDS_COMMON.iter().any(|k| k.eq_ignore_ascii_case(word))
}

fn looks_like_function_call_context(prefix: &str) -> bool {
    // Be conservative: only re-color as a function when nothing meaningful
    // immediately precedes the identifier (typical `name(` or `  name(`).
    let trimmed = prefix.trim_end();
    if trimmed.is_empty() {
        return true;
    }
    let last_char = trimmed.chars().next_back().unwrap();
    matches!(
        last_char,
        '(' | ','
            | ';'
            | '{'
            | '}'
            | '|'
            | '&'
            | '+'
            | '-'
            | '*'
            | '/'
            | '%'
            | '='
            | '<'
            | '>'
            | '!'
            | '?'
            | ':'
            | '\n'
            | '['
            | ']'
    )
}

fn is_ident_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_' || ch == '$'
}

fn is_ident_cont(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn lang_is_c_like(lang: &str) -> bool {
    matches!(
        lang,
        "" | "rust"
            | "rs"
            | "c"
            | "cpp"
            | "c++"
            | "cc"
            | "cxx"
            | "h"
            | "hpp"
            | "javascript"
            | "js"
            | "typescript"
            | "ts"
            | "tsx"
            | "jsx"
            | "go"
            | "java"
            | "kotlin"
            | "swift"
    )
}

fn lang_is_shell_like(lang: &str) -> bool {
    matches!(
        lang,
        "" | "sh" | "bash" | "zsh" | "shell" | "py" | "python" | "rb" | "ruby"
    )
}

fn render_paragraph(lines: &[String], width: usize) -> Vec<Line<'static>> {
    let joined = lines.join(" ");
    wrap_styled_line(render_inline(&joined), width)
}

fn wrap_styled_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
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

fn render_inline(text: &str) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let plain_style = Style::default().fg(PLAIN_FG);
    parse_inline(text, plain_style, &mut spans);
    Line::from(spans)
}

fn parse_inline(text: &str, base_style: Style, spans: &mut Vec<Span<'static>>) {
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
            if delimiter.starts_with('_') && idx > 0 {
                if text[..idx]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_alphanumeric)
                {
                    continue;
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn flatten(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_simple_paragraph() {
        let lines = render("hello world", 80);
        assert_eq!(flatten(&lines), "hello world");
    }

    #[test]
    fn renders_horizontal_rule_to_the_available_width() {
        let lines = render("上方\n---\n下方", 12);
        assert_eq!(flatten(&lines), "上方\n────────────\n下方");
        assert_eq!(lines[1].spans[0].style.fg, Some(RULE_FG));
    }

    #[test]
    fn recognizes_standard_horizontal_rule_forms_only() {
        for valid in ["---", "***", "___", "- - -", "  * * * *"] {
            assert!(is_horizontal_rule(valid), "应识别为分隔线：{valid}");
        }
        for invalid in ["--", "- -", "--- text", "-*‑", "    ---"] {
            assert!(!is_horizontal_rule(invalid), "不应识别为分隔线：{invalid}");
        }
    }

    #[test]
    fn renders_inline_code_with_backticks() {
        let lines = render("run `cargo build` to compile", 80);
        let flat = flatten(&lines);
        assert!(flat.contains("cargo build"));
    }

    #[test]
    fn renders_bold_and_italic_without_markdown_delimiters() {
        let lines = render("**我的能力范围：** __重点__ *斜体* _强调_ ***粗斜体***", 80);
        let flat = flatten(&lines);
        assert_eq!(flat, "我的能力范围： 重点 斜体 强调 粗斜体");
        assert!(!flat.contains('*'));
        assert!(!flat.contains('_'));

        let spans = &lines[0].spans;
        assert!(spans.iter().any(|span| {
            span.content.contains("我的能力范围")
                && span.style.add_modifier.contains(Modifier::BOLD)
        }));
        assert!(spans.iter().any(|span| {
            span.content.contains("斜体") && span.style.add_modifier.contains(Modifier::ITALIC)
        }));
        assert!(spans.iter().any(|span| {
            span.content.contains("粗斜体")
                && span
                    .style
                    .add_modifier
                    .contains(Modifier::BOLD | Modifier::ITALIC)
        }));
    }

    #[test]
    fn emphasis_style_survives_terminal_wrapping() {
        let lines = render("前缀 **这是一段跨越自动换行的粗体文字** 后缀", 10);
        assert!(lines.len() > 1);
        let bold_text = lines
            .iter()
            .flat_map(|line| &line.spans)
            .filter(|span| span.style.add_modifier.contains(Modifier::BOLD))
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(bold_text, "这是一段跨越自动换行的粗体文字");
        assert!(!flatten(&lines).contains("**"));
    }

    #[test]
    fn inline_code_does_not_parse_emphasis_markers() {
        let lines = render(r"`**原样代码**` 与 \*原样星号\*", 80);
        assert_eq!(flatten(&lines), "**原样代码** 与 *原样星号*");
    }

    #[test]
    fn renders_pipe_table_with_alignment() {
        let md = "\
| Name | Score | Pass |
| :--- | ----: | :---: |
| alice | 92 | yes |
| bob | 71 | no |";
        let lines = render(md, 80);
        let flat = flatten(&lines);
        for needle in [
            "Name", "Score", "Pass", "alice", "92", "yes", "bob", "71", "no",
        ] {
            assert!(flat.contains(needle), "missing {needle} in:\n{flat}");
        }
        assert!(flat.contains('│'), "table borders missing");
        assert!(flat.contains('─'), "table separators missing");
    }

    #[test]
    fn code_block_renders_with_syntax_styling() {
        let md = "```rust\nfn main() { let x = 42; }\n```";
        let lines = render(md, 80);
        // Code lines should be wrapped in a box.
        let flat = flatten(&lines);
        assert!(flat.contains("fn"), "fn missing:\n{flat}");
        assert!(flat.contains("main"), "main missing:\n{flat}");
        assert!(flat.contains("42"), "42 missing:\n{flat}");
        assert!(flat.contains("│"), "code box borders missing");
    }

    #[test]
    fn code_block_highlights_strings_and_comments() {
        // The whole-line comment handler absorbs the digits after `//`.
        let comment_line = highlight_code("// greet and 42", "rust");
        assert!(
            comment_line
                .iter()
                .any(|s| s.style.fg == Some(COMMENT_FG) && s.content.contains("//")),
            "comment line not styled"
        );

        // Numbers outside comments get their own color.
        let mixed = highlight_code("let n = 42;", "rust");
        assert!(
            mixed
                .iter()
                .any(|s| s.style.fg == Some(NUMBER_FG) && s.content == "42"),
            "number not styled: {mixed:?}"
        );

        // Strings inside code are highlighted regardless of language.
        let string_line = highlight_code(r#"echo "hello""#, "bash");
        assert!(
            string_line
                .iter()
                .any(|s| s.style.fg == Some(STRING_FG) && s.content.contains("hello")),
            "string not highlighted: {string_line:?}"
        );
    }

    #[test]
    fn paragraph_wraps_at_width_boundary() {
        let lines = render("aaa bbb ccc ddd", 7);
        assert!(lines.len() >= 2, "expected wrap, got {} lines", lines.len());
    }

    #[test]
    fn is_table_row_detects_pipe_lines() {
        assert!(is_table_row("| a | b |"));
        assert!(is_table_row("a | b"));
        assert!(!is_table_row("plain text"));
        assert!(!is_table_row("|"));
    }

    #[test]
    fn is_table_separator_requires_dashes() {
        assert!(is_table_separator("| --- | :---: |"));
        assert!(is_table_separator("--- | ---"));
        assert!(!is_table_separator("| not | separator |"));
    }
}
