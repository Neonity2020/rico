//! Fenced code block syntax highlighting and boxed frame rendering.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use super::table::display_width;

pub const COMMENT_FG: Color = Color::Rgb(127, 139, 154);
pub const STRING_FG: Color = Color::Rgb(152, 195, 121);
pub const NUMBER_FG: Color = Color::Rgb(209, 154, 102);
pub const KEYWORD_FG: Color = Color::Rgb(198, 120, 221);
pub const FUNCTION_FG: Color = Color::Rgb(97, 175, 239);
pub const TYPE_FG: Color = Color::Rgb(229, 192, 123);
pub const PLAIN_FG: Color = Color::White;

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

pub fn render_code_block(code: &str, lang: Option<&str>, width: usize) -> Vec<Line<'static>> {
    let tag = lang.unwrap_or("");
    let label = if tag.is_empty() {
        " 代码 ".to_string()
    } else {
        format!(" {tag} ")
    };
    let border_style = Style::default().fg(Color::DarkGray);
    let mut out = Vec::new();

    let clean_code = code.replace('\t', "    ");
    let highlighted: Vec<Vec<Span<'static>>> = clean_code
        .split('\n')
        .map(|line| highlight_code(line, tag))
        .collect();

    let label_w = display_width(&label);
    let mut max_line_w = label_w + 2;
    for line in &highlighted {
        let w: usize = line.iter().map(|s| display_width(s.content.as_ref())).sum();
        max_line_w = max_line_w.max(w);
    }

    // Box width: fits content while bounded by available width.
    let box_width = (max_line_w + 4).max(label_w + 6).min(width);
    let inner_width = box_width.saturating_sub(4); // 2 on left ("│ "), 2 on right (" │")

    // Top border: ┌── label ──┐
    out.push(frame_top_border(&label, box_width, border_style));

    // Code lines.
    for line_spans in highlighted {
        let line_w: usize = line_spans.iter().map(|s| display_width(s.content.as_ref())).sum();
        if line_w <= inner_width {
            out.push(code_line(line_spans, line_w, inner_width, border_style));
        } else {
            out.extend(wrap_code_line(line_spans, inner_width, border_style));
        }
    }

    // Bottom border: └───────┘
    out.push(frame_bottom_border(box_width, border_style));
    out
}

fn frame_top_border(label: &str, width: usize, style: Style) -> Line<'static> {
    let label_w = display_width(label);
    if width < label_w + 4 {
        let dashes = "─".repeat(width.saturating_sub(2));
        return Line::from(Span::styled(format!("┌{dashes}┐"), style));
    }
    let remaining = width - 2 - label_w;
    let left = remaining / 2;
    let right = remaining - left;
    let dashes_left = "─".repeat(left);
    let dashes_right = "─".repeat(right);
    Line::from(Span::styled(
        format!("┌{dashes_left}{label}{dashes_right}┐"),
        style,
    ))
}

fn frame_bottom_border(width: usize, style: Style) -> Line<'static> {
    let dashes = "─".repeat(width.saturating_sub(2));
    Line::from(Span::styled(format!("└{dashes}┘"), style))
}

fn code_line(
    spans: Vec<Span<'static>>,
    used_width: usize,
    inner_width: usize,
    border_style: Style,
) -> Line<'static> {
    let pad = inner_width.saturating_sub(used_width);
    let mut line_spans = Vec::with_capacity(spans.len() + 3);
    line_spans.push(Span::styled("│ ", border_style));
    line_spans.extend(spans);
    if pad > 0 {
        line_spans.push(Span::raw(" ".repeat(pad)));
    }
    line_spans.push(Span::styled(" │", border_style));
    Line::from(line_spans)
}

fn wrap_code_line(
    spans: Vec<Span<'static>>,
    inner_width: usize,
    border_style: Style,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut current_width: usize = 0;

    for span in spans {
        let style = span.style;
        let text = span.content.into_owned();
        let mut buf = String::new();

        for ch in text.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if current_width + cw > inner_width && (!current_spans.is_empty() || !buf.is_empty()) {
                if !buf.is_empty() {
                    current_spans.push(Span::styled(std::mem::take(&mut buf), style));
                }
                out.push(code_line(
                    std::mem::take(&mut current_spans),
                    current_width,
                    inner_width,
                    border_style,
                ));
                current_width = 0;
            }
            buf.push(ch);
            current_width += cw;
        }
        if !buf.is_empty() {
            current_spans.push(Span::styled(buf, style));
        }
    }

    if !current_spans.is_empty() || out.is_empty() {
        out.push(code_line(
            current_spans,
            current_width,
            inner_width,
            border_style,
        ));
    }
    out
}

pub fn highlight_code(line: &str, lang: &str) -> Vec<Span<'static>> {
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
