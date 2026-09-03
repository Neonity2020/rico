//! Ratatui rendering logic for dialog transcript, welcome banner, status, editor and footer.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use super::{
    app::{App, Entry, Philosophy, PHILOSOPHIES},
    editor::{cursor_position, editor_rows, join_sides, truncate_to_width, wrap_text},
    selection::render_selection,
};

pub const ACCENT: Color = Color::Rgb(110, 190, 180);
pub const MUTED: Color = Color::Rgb(120, 126, 138);
pub const USER_BG: Color = Color::Rgb(48, 52, 61);
pub const TOOL_BG: Color = Color::Rgb(38, 42, 49);
pub const ERROR: Color = Color::Rgb(224, 108, 117);
pub const MAX_EDITOR_ROWS: u16 = 6;
pub const TOOL_PREVIEW_LINES: usize = 8;
pub const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub fn render(frame: &mut ratatui::Frame<'_>, app: &mut App) {
    let area = frame.area();
    let wrap_width = area.width.saturating_sub(1).max(1) as usize;
    let editor_rows = editor_rows(&app.input, wrap_width)
        .clamp(1, MAX_EDITOR_ROWS as usize) as u16;
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(2),
            Constraint::Length(editor_rows + 2),
            Constraint::Length(2),
        ])
        .split(area);
    render_conversation(frame, app, layout[0]);
    render_status(frame, app, layout[1]);
    render_editor(frame, app, layout[2]);
    render_footer(frame, app, layout[3]);
}

pub fn render_conversation(frame: &mut ratatui::Frame<'_>, app: &mut App, area: Rect) {
    let width = area.width.max(1) as usize;
    let mut lines = Vec::new();
    if app.entries.is_empty() && app.streaming.is_empty() {
        let model = if app.provider.is_empty() {
            app.model.clone()
        } else {
            format!("{} · {}", app.provider, app.model)
        };
        lines.extend(render_welcome(
            &model,
            PHILOSOPHIES[app.philosophy_index],
            width,
        ));
    }
    for entry in &app.entries {
        lines.extend(render_entry(entry, width));
    }
    if !app.streaming.is_empty() {
        lines.push(Line::raw(""));
        let visible = visible_assistant_text(&app.streaming);
        lines.extend(pad_lines(
            crate::markdown::render(&visible, width.saturating_sub(4)),
            2,
        ));
    }
    if let Some((name, args)) = &app.pending_tool {
        lines.extend(render_tool(name, args, None, width));
    }

    let viewport = area.height as usize;
    app.transcript_scroll.update_layout(lines.len(), viewport);
    app.transcript_area = area;
    app.transcript_lines = lines.iter().map(line_text).collect();
    let start = app.transcript_scroll.top;
    let visible = lines
        .into_iter()
        .skip(start)
        .take(viewport)
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(Text::from(visible)), area);
    render_selection(frame, app);
}

pub fn render_welcome(model: &str, philosophy: Philosophy, width: usize) -> Vec<Line<'static>> {
    const LOGO: [&str; 6] = [
        "██████╗ ██╗ ██████╗ ██████╗ ",
        "██╔══██╗██║██╔════╝██╔═══██╗",
        "██████╔╝██║██║     ██║   ██║",
        "██╔══██╗██║██║     ██║   ██║",
        "██║  ██║██║╚██████╗╚██████╔╝",
        "╚═╝  ╚═╝╚═╝ ╚═════╝ ╚═════╝ ",
    ];
    const LOGO_COLORS: [Color; 6] = [
        Color::Rgb(86, 214, 192),
        Color::Rgb(91, 205, 190),
        Color::Rgb(96, 196, 188),
        Color::Rgb(101, 187, 186),
        Color::Rgb(106, 178, 184),
        Color::Rgb(111, 169, 182),
    ];

    let mut lines = Vec::new();
    if width >= 48 {
        for (row, color) in LOGO.into_iter().zip(LOGO_COLORS) {
            lines.push(Line::from(Span::styled(
                row,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )));
        }
    } else {
        lines.push(Line::from(vec![
            Span::styled(
                "rico",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" v{}", env!("CARGO_PKG_VERSION")),
                Style::default().fg(MUTED),
            ),
        ]));
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        truncate_to_width("◆ 使用 Rust 构建，追求极致性能。", width),
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        truncate_to_width("  原生终端，毫秒必争。", width),
        Style::default().fg(MUTED),
    )));
    lines.push(Line::raw(""));
    lines.extend(render_philosophy(philosophy, width));
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("模型 ", Style::default().fg(MUTED)),
        Span::styled(model.to_owned(), Style::default().fg(Color::White)),
        Span::styled(
            format!("  ·  rico v{}", env!("CARGO_PKG_VERSION")),
            Style::default().fg(MUTED),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        "输入任务开始对话。",
        Style::default().fg(MUTED),
    )));
    lines
}

pub fn render_philosophy(philosophy: Philosophy, width: usize) -> Vec<Line<'static>> {
    let source = format!("{} ({})", philosophy.principle, philosophy.original);
    if width < 48 {
        let prefix = "哲思 · ";
        return vec![
            Line::from(vec![
                Span::styled(prefix, Style::default().fg(ACCENT)),
                Span::styled(
                    truncate_to_width(
                        &source,
                        width.saturating_sub(UnicodeWidthStr::width(prefix)),
                    ),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(Span::styled(
                truncate_to_width(philosophy.meaning, width),
                Style::default().fg(MUTED),
            )),
        ];
    }

    let box_width = width.min(76);
    let title = " 编程哲学 ";
    let top_fill = box_width.saturating_sub(3 + UnicodeWidthStr::width(title));
    let top = format!("╭─{title}{}╮", "─".repeat(top_fill));
    let bottom = format!("╰{}╯", "─".repeat(box_width.saturating_sub(2)));
    vec![
        Line::from(Span::styled(top, Style::default().fg(ACCENT))),
        philosophy_box_line(
            &source,
            box_width,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        philosophy_box_line(philosophy.meaning, box_width, Style::default().fg(MUTED)),
        Line::from(Span::styled(bottom, Style::default().fg(ACCENT))),
    ]
}

pub fn philosophy_box_line(text: &str, width: usize, style: Style) -> Line<'static> {
    let inner_width = width.saturating_sub(4);
    let text = truncate_to_width(text, inner_width);
    let padding = inner_width.saturating_sub(UnicodeWidthStr::width(text.as_str()));
    Line::from(vec![
        Span::styled("│ ", Style::default().fg(ACCENT)),
        Span::styled(text, style),
        Span::raw(" ".repeat(padding)),
        Span::styled(" │", Style::default().fg(ACCENT)),
    ])
}

pub fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

pub fn render_status(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    if app.busy {
        let spinner = SPINNER[(app.tick / 2) % SPINNER.len()];
        let text = Line::from(vec![
            Span::styled(format!("{spinner} "), Style::default().fg(ACCENT)),
            Span::styled(app.status.label(), Style::default().fg(MUTED)),
            Span::styled("  Ctrl-C 退出", Style::default().fg(MUTED)),
        ]);
        frame.render_widget(Paragraph::new(vec![Line::raw(""), text]), area);
    }
}

pub fn render_entry(entry: &Entry, width: usize) -> Vec<Line<'static>> {
    match entry {
        Entry::User(text) => {
            const MAX_BUBBLE_WIDTH: usize = 110;
            let available = width.saturating_sub(4).max(1);
            let target_width = available.min(MAX_BUBBLE_WIDTH);
            let text_lines: Vec<String> = text
                .lines()
                .flat_map(|line| wrap_text(line, target_width))
                .collect();
            let max_line_w = text_lines
                .iter()
                .map(|l| UnicodeWidthStr::width(l.as_str()))
                .max()
                .unwrap_or(0);
            let inner = max_line_w.max(1);
            let bubble_width = inner + 4;
            let background = Style::default().bg(USER_BG).fg(Color::White);
            let mut lines = vec![Line::from(Span::styled(" ".repeat(bubble_width), background))];
            for text_line in text_lines {
                let padding = inner.saturating_sub(UnicodeWidthStr::width(text_line.as_str()));
                lines.push(Line::from(Span::styled(
                    format!("  {text_line}{}  ", " ".repeat(padding)),
                    background,
                )));
            }
            lines.push(Line::from(Span::styled(" ".repeat(bubble_width), background)));
            lines
        }
        Entry::Assistant(text) => {
            let mut lines = vec![Line::raw("")];
            let visible = visible_assistant_text(text);
            lines.extend(pad_lines(
                crate::markdown::render(visible.trim(), width.saturating_sub(4)),
                2,
            ));
            lines
        }
        Entry::Tool { name, args, result } => render_tool(name, args, result.as_deref(), width),
        Entry::Info(text) => vec![
            Line::raw(""),
            Line::from(Span::styled(
                format!("  {text}"),
                Style::default().fg(ACCENT),
            )),
        ],
        Entry::Error(text) => vec![
            Line::raw(""),
            Line::from(Span::styled(
                format!("  错误：{text}"),
                Style::default().fg(ERROR),
            )),
        ],
    }
}

pub fn render_tool(
    name: &str,
    args: &str,
    result: Option<&str>,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::raw("")];
    let summary = tool_summary(args);
    lines.push(
        Line::from(vec![
            Span::styled(
                "  ● ",
                Style::default().fg(if result.is_some() {
                    ACCENT
                } else {
                    Color::Yellow
                }),
            ),
            Span::styled(
                name.to_owned(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {summary}"), Style::default().fg(MUTED)),
        ])
        .style(Style::default().bg(TOOL_BG)),
    );
    if let Some(output) = result.filter(|value| !value.trim().is_empty()) {
        let output_lines = output.lines().collect::<Vec<_>>();
        for line in output_lines.iter().take(TOOL_PREVIEW_LINES) {
            let value = truncate_to_width(line, width.saturating_sub(6));
            lines.push(Line::from(vec![
                Span::styled("  │ ", Style::default().fg(MUTED)),
                Span::styled(value, Style::default().fg(MUTED)),
            ]));
        }
        if output_lines.len() > TOOL_PREVIEW_LINES {
            lines.push(Line::from(Span::styled(
                format!("  └ … 还有 {} 行", output_lines.len() - TOOL_PREVIEW_LINES),
                Style::default().fg(MUTED),
            )));
        }
    }
    lines
}

pub fn render_editor(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let border_color = if app.busy { MUTED } else { ACCENT };
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(border_color));
    let placeholder = if app.input.is_empty() && !app.busy {
        "输入消息"
    } else {
        ""
    };
    let masked_input;
    let content = if app.login_provider.is_some() {
        masked_input = "•".repeat(app.input.chars().count());
        masked_input.as_str()
    } else if placeholder.is_empty() {
        app.input.as_str()
    } else {
        placeholder
    };
    let style = if placeholder.is_empty() {
        Style::default()
    } else {
        Style::default().fg(MUTED)
    };
    let width = area.width.max(1) as usize;
    let wrap_width = width.saturating_sub(1).max(1);
    let wrapped_lines = wrap_text(content, wrap_width);
    let text_widget = Text::from(wrapped_lines.into_iter().map(Line::raw).collect::<Vec<_>>());
    let paragraph = Paragraph::new(text_widget)
        .style(style)
        .block(block);
    frame.render_widget(paragraph, area);

    if !app.busy {
        let (column, row) = cursor_position(&app.input, app.cursor, wrap_width);
        let x = area.x + column as u16;
        let y = area.y + 1 + row as u16;
        if x < area.right() && y < area.bottom() {
            frame.set_cursor_position((x, y));
        }
    }
}

pub fn render_footer(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let session = app
        .session_path
        .as_ref()
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("临时会话");
    let step = app
        .step
        .map(|(current, total)| match total {
            Some(total) => format!(" · 步骤 {current}/{total}"),
            None => format!(" · 步骤 {current}"),
        })
        .unwrap_or_default();
    let cache_info = if app.cache_stats.requests_count > 0 {
        let stats = &app.cache_stats;
        if stats.requests_count > 1 {
            format!(
                " · 缓存命中 {:.1}% (本轮 {:.1}%, {}/{})",
                stats.overall_hit_rate(),
                stats.latest_hit_rate(),
                stats.latest_cached_tokens,
                stats.latest_prompt_tokens
            )
        } else {
            format!(
                " · 缓存命中 {:.1}% ({}/{})",
                stats.overall_hit_rate(),
                stats.latest_cached_tokens,
                stats.latest_prompt_tokens
            )
        }
    } else {
        String::new()
    };
    let provider = if app.provider.is_empty() {
        app.model.clone()
    } else {
        format!("{} · {}", app.provider, app.model)
    };
    let left = format!("{} · {session} · {provider}", app.cwd);
    let right = format!(
        "约 {} 词元{cache_info}{step} · {}",
        app.tokens,
        app.status.label()
    );
    let first = join_sides(&left, &right, area.width as usize);
    let hints = if app.busy {
        "Ctrl-C 退出 · PgUp/PgDn 滚动"
    } else {
        "Enter 发送 · Shift-Enter 换行 · /login 登录 · /provider 切换 · /cache 统计 · ↑/↓ 历史 · Ctrl-L 清空 · Ctrl-D 退出"
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(first, Style::default().fg(MUTED))),
            Line::from(Span::styled(
                truncate_to_width(hints, area.width as usize),
                Style::default().fg(MUTED),
            )),
        ]),
        area,
    );
}

pub fn pad_lines(lines: Vec<Line<'static>>, padding: usize) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|line| {
            let mut spans = vec![Span::raw(" ".repeat(padding))];
            spans.extend(line.spans);
            Line::from(spans)
        })
        .collect()
}

pub fn visible_assistant_text(text: &str) -> String {
    let mut visible = String::new();
    let mut rest = text;
    loop {
        let Some(start) = rest.find("<think>") else {
            visible.push_str(rest);
            break;
        };
        visible.push_str(&rest[..start]);
        let after_start = &rest[start + "<think>".len()..];
        let Some(end) = after_start.find("</think>") else {
            break;
        };
        rest = &after_start[end + "</think>".len()..];
    }
    visible.trim_start_matches(['\r', '\n']).to_owned()
}

pub fn tool_summary(args: &str) -> String {
    serde_json::from_str::<serde_json::Value>(args)
        .ok()
        .and_then(|value| {
            ["command", "path", "file_path"]
                .iter()
                .find_map(|key| value.get(key)?.as_str().map(str::to_owned))
        })
        .unwrap_or_else(|| args.split_whitespace().collect::<Vec<_>>().join(" "))
        .chars()
        .take(120)
        .collect()
}
