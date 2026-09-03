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

pub mod code;
pub mod inline;
pub mod list;
pub mod table;

use ratatui::{
    style::Style,
    text::{Line, Span},
};

use self::{
    code::render_code_block,
    inline::{is_horizontal_rule, render_paragraph, RULE_FG},
    list::{parse_list_item, render_list},
    table::{is_table_row, is_table_separator, parse_alignments, parse_row, render_table},
};

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
            let aligns = parse_alignments(&lines[i + 1]);
            i += 2; // skip header and separator
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

        let stripped = raw.trim();
        if let Some(rest) = stripped.strip_prefix("### ") {
            out.push(Line::raw(""));
            let mut rendered = render_paragraph(&[rest.to_string()], width);
            for line in &mut rendered {
                for span in &mut line.spans {
                    span.style = span.style.add_modifier(ratatui::style::Modifier::BOLD);
                }
            }
            out.extend(rendered);
            i += 1;
            continue;
        }
        if let Some(rest) = stripped.strip_prefix("## ") {
            out.push(Line::raw(""));
            let mut rendered = render_paragraph(&[rest.to_string()], width);
            for line in &mut rendered {
                for span in &mut line.spans {
                    span.style = span
                        .style
                        .add_modifier(ratatui::style::Modifier::BOLD)
                        .fg(ratatui::style::Color::Cyan);
                }
            }
            out.extend(rendered);
            i += 1;
            continue;
        }
        if let Some(rest) = stripped.strip_prefix("# ") {
            out.push(Line::raw(""));
            let mut rendered = render_paragraph(&[rest.to_string()], width);
            for line in &mut rendered {
                for span in &mut line.spans {
                    span.style = span
                        .style
                        .add_modifier(ratatui::style::Modifier::BOLD | ratatui::style::Modifier::UNDERLINED);
                }
            }
            out.extend(rendered);
            i += 1;
            continue;
        }

        // List block: - , * , + , 1. , - [ ] etc.
        if parse_list_item(raw).is_some() {
            let mut list_lines = Vec::new();
            while i < lines.len() {
                let current = lines[i].as_str();
                if current.trim().is_empty() {
                    break;
                }
                if current.trim_start().starts_with("```") || is_horizontal_rule(current) {
                    break;
                }
                let cur_trim = current.trim_start();
                if cur_trim.starts_with("# ")
                    || cur_trim.starts_with("## ")
                    || cur_trim.starts_with("### ")
                {
                    break;
                }
                if is_table_row(current)
                    && i + 1 < lines.len()
                    && is_table_separator(lines[i + 1].as_str())
                {
                    break;
                }
                if parse_list_item(current).is_some()
                    || current.starts_with("  ")
                    || current.starts_with('\t')
                {
                    list_lines.push(current.to_string());
                    i += 1;
                } else {
                    break;
                }
            }
            if !out.is_empty() && !out.last().is_some_and(|l| l.spans.is_empty()) {
                out.push(Line::raw(""));
            }
            out.extend(render_list(&list_lines, width));
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
            let cur_trim = current.trim_start();
            if cur_trim.starts_with("# ")
                || cur_trim.starts_with("## ")
                || cur_trim.starts_with("### ")
            {
                break;
            }
            if parse_list_item(current).is_some() {
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

#[cfg(test)]
mod tests {
    use ratatui::style::Modifier;

    use super::{
        code::{highlight_code, COMMENT_FG, NUMBER_FG, STRING_FG},
        inline::{is_horizontal_rule, RULE_FG},
        table::{display_width, is_table_row, is_table_separator},
        *,
    };

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
    fn renders_headings_with_styles() {
        let md = "### 3. 处理多行文本建议\n普通段落";
        let lines = render(md, 80);
        let flat = flatten(&lines);
        assert!(flat.contains("3. 处理多行文本建议"));
        assert!(!flat.contains("###"));
        let has_bold = lines.iter().flat_map(|l| &l.spans).any(|s| {
            s.content.contains("处理多行文本建议") && s.style.add_modifier.contains(Modifier::BOLD)
        });
        assert!(has_bold, "heading must be styled with BOLD modifier");
    }

    #[test]
    fn renders_markdown_lists_with_bullets_and_hanging_indent() {
        let md = "- 表格只放短文本（年份、状态、简短标题）。\n- 详细说明（如大事记）采用 Master-Detail：\n  光标选中行。\n1. 第一步\n2. 第二步\n- [x] 完成任务\n- [ ] 未完成任务";
        let lines = render(md, 80);
        let flat = flatten(&lines);
        assert!(flat.contains("• 表格只放短文本"));
        assert!(flat.contains("1. 第一步"));
        assert!(flat.contains("2. 第二步"));
        assert!(flat.contains("☑ 完成任务"));
        assert!(flat.contains("☐ 未完成任务"));
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

    #[test]
    fn code_block_cjk_lines_align_perfectly() {
        let md = "```\nsrc/\n├── main.rs       211 行入口：CLI 解析、配置加载、CLI/TUI 分派\n├── agent.rs      312 行   Agent 核心循环、上下文压缩、事件 sink\n└── tui.rs       1659 行   ratatui 渲染 + crossterm 输入 + Agent 事件通道\n```";
        let lines = render(md, 80);
        let block_lines: Vec<_> = lines
            .iter()
            .filter(|l| !l.spans.is_empty() && !l.spans[0].content.is_empty())
            .collect();
        assert!(!block_lines.is_empty());
        let expected_w = block_lines[0]
            .spans
            .iter()
            .map(|s| display_width(s.content.as_ref()))
            .sum::<usize>();
        for (i, line) in block_lines.iter().enumerate() {
            let w: usize = line
                .spans
                .iter()
                .map(|s| display_width(s.content.as_ref()))
                .sum();
            assert_eq!(
                w, expected_w,
                "Line {} display width {} mismatch expected {}",
                i, w, expected_w
            );
        }
    }

    #[test]
    fn table_cjk_and_alignment_lines_align_perfectly() {
        let md = "\
| 模块名称 | 代码行数 | 功能描述 |
| :--- | ----: | :--- |
| main.rs | 211 | CLI 解析、配置加载与分派 |
| provider.rs | 407 | OpenAI 兼容 SSE 流式协议与 tool_calls 解析 |
| tui.rs | 1659 | ratatui 渲染 + crossterm 输入通道 |";
        let lines = render(md, 80);
        let table_lines: Vec<_> = lines
            .iter()
            .filter(|l| !l.spans.is_empty() && !l.spans[0].content.is_empty())
            .collect();
        assert!(table_lines.len() >= 5);
        let expected_w = table_lines[0]
            .spans
            .iter()
            .map(|s| display_width(s.content.as_ref()))
            .sum::<usize>();
        for (i, line) in table_lines.iter().enumerate() {
            let w: usize = line
                .spans
                .iter()
                .map(|s| display_width(s.content.as_ref()))
                .sum();
            assert_eq!(
                w, expected_w,
                "Table line {} display width {} mismatch expected {}",
                i, w, expected_w
            );
        }
    }
}
