//! Pi-inspired terminal interface for rico.
//!
//! The UI is deliberately quiet: conversation first, a bordered editor near
//! the bottom, and two compact footer lines. All editing uses Unicode grapheme
//! boundaries, while cursor placement uses terminal cell widths.

use std::{
    env,
    io::{stdout, Write},
    path::PathBuf,
    process::{self, Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use base64::Engine;
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::StreamExt;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
    Terminal,
};
use scopeguard::defer;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::agent::{Agent, AgentEvent};

const ACCENT: Color = Color::Rgb(110, 190, 180);
const MUTED: Color = Color::Rgb(120, 126, 138);
const USER_BG: Color = Color::Rgb(48, 52, 61);
const TOOL_BG: Color = Color::Rgb(38, 42, 49);
const ERROR: Color = Color::Rgb(224, 108, 117);
const MAX_EDITOR_ROWS: u16 = 6;
const TOOL_PREVIEW_LINES: usize = 8;
const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

#[derive(Debug, Clone, Copy)]
struct Philosophy {
    principle: &'static str,
    original: &'static str,
    meaning: &'static str,
}

const PHILOSOPHIES: &[Philosophy] = &[
    Philosophy {
        principle: "DRY",
        original: "Don't Repeat Yourself",
        meaning: "消除知识重复，让每条规则只有一个权威来源。",
    },
    Philosophy {
        principle: "KISS",
        original: "Keep It Simple, Stupid",
        meaning: "简单不是少做，而是拒绝不必要的复杂度。",
    },
    Philosophy {
        principle: "YAGNI",
        original: "You Aren't Gonna Need It",
        meaning: "只构建当下真正需要的能力。",
    },
    Philosophy {
        principle: "UNIX",
        original: "Do one thing and do it well",
        meaning: "让组件专注、可组合，并把一件事做到极致。",
    },
    Philosophy {
        principle: "DIJKSTRA",
        original: "Simplicity is prerequisite for reliability",
        meaning: "可靠性始于可理解、可验证的简单设计。",
    },
    Philosophy {
        principle: "KNUTH",
        original: "Premature optimization is the root of all evil",
        meaning: "先测量，再优化真正的瓶颈。",
    },
    Philosophy {
        principle: "TYPES",
        original: "Make invalid states unrepresentable",
        meaning: "用类型约束错误，让非法状态无法构造。",
    },
    Philosophy {
        principle: "SICP",
        original: "Programs must be written for people to read",
        meaning: "代码首先服务于读者，其次才交给机器执行。",
    },
];

fn select_philosophy_index() -> usize {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as usize;
    (time ^ process::id() as usize).wrapping_mul(0x9e37_79b1) % PHILOSOPHIES.len()
}

#[derive(Debug)]
enum UserCommand {
    Submit(String),
    Reset,
    Shutdown,
}

#[derive(Debug, Clone)]
enum Entry {
    User(String),
    Assistant(String),
    Info(String),
    Error(String),
    Tool {
        name: String,
        args: String,
        result: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Idle,
    Thinking,
    RunningTool(String),
    Compacting,
}

impl Status {
    fn label(&self) -> String {
        match self {
            Self::Idle => "就绪".into(),
            Self::Thinking => "正在思考…".into(),
            Self::RunningTool(name) => format!("正在运行 {name}…"),
            Self::Compacting => "正在压缩上下文…".into(),
        }
    }
}

struct App {
    entries: Vec<Entry>,
    streaming: String,
    pending_tool: Option<(String, String)>,
    input: String,
    cursor: usize,
    history: Vec<String>,
    history_index: Option<usize>,
    history_draft: String,
    transcript_scroll: ScrollViewState,
    transcript_area: Rect,
    transcript_lines: Vec<String>,
    selection_anchor: Option<SelectionPoint>,
    selection_focus: Option<SelectionPoint>,
    selecting: bool,
    philosophy_index: usize,
    model: String,
    cwd: String,
    session_path: Option<PathBuf>,
    tokens: usize,
    status: Status,
    step: Option<(usize, usize)>,
    busy: bool,
    should_quit: bool,
    tick: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SelectionPoint {
    row: usize,
    column: usize,
}

#[derive(Default)]
struct ScrollViewState {
    top: usize,
    content_height: usize,
    viewport_height: usize,
    follow_end: bool,
}

impl ScrollViewState {
    fn following_end() -> Self {
        Self {
            follow_end: true,
            ..Self::default()
        }
    }

    fn update_layout(&mut self, content_height: usize, viewport_height: usize) {
        self.content_height = content_height;
        self.viewport_height = viewport_height;
        let end = content_height.saturating_sub(viewport_height);
        self.top = if self.follow_end {
            end
        } else {
            self.top.min(end)
        };
    }

    fn scroll_by(&mut self, delta: i32) {
        let end = self.content_height.saturating_sub(self.viewport_height);
        self.top = (self.top as i64 + delta as i64).clamp(0, end as i64) as usize;
        self.follow_end = self.top == end;
    }

    fn scroll_to_end(&mut self) {
        self.follow_end = true;
        self.top = self.content_height.saturating_sub(self.viewport_height);
    }
}

impl App {
    fn new(model: String, session_path: Option<PathBuf>, cwd: String) -> Self {
        Self {
            entries: Vec::new(),
            streaming: String::new(),
            pending_tool: None,
            input: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_index: None,
            history_draft: String::new(),
            transcript_scroll: ScrollViewState::following_end(),
            transcript_area: Rect::default(),
            transcript_lines: Vec::new(),
            selection_anchor: None,
            selection_focus: None,
            selecting: false,
            philosophy_index: select_philosophy_index(),
            model,
            cwd,
            session_path,
            tokens: 0,
            status: Status::Idle,
            step: None,
            busy: false,
            should_quit: false,
            tick: 0,
        }
    }

    fn apply_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Delta(delta) => {
                self.streaming.push_str(&delta);
                self.status = Status::Thinking;
            }
            AgentEvent::ToolStart { name, args } => {
                self.flush_streaming();
                self.pending_tool = Some((name.clone(), args));
                self.status = Status::RunningTool(name);
            }
            AgentEvent::ToolResult { output } => {
                if let Some((name, args)) = self.pending_tool.take() {
                    self.entries.push(Entry::Tool {
                        name,
                        args,
                        result: Some(output),
                    });
                }
            }
            AgentEvent::Step { current, total } => {
                self.step = Some((current, total));
                if !matches!(self.status, Status::RunningTool(_)) {
                    self.status = Status::Thinking;
                }
            }
            AgentEvent::Compacting => self.status = Status::Compacting,
            AgentEvent::Complete => {
                self.flush_streaming();
                self.finish_turn();
            }
            AgentEvent::Error(message) => {
                self.flush_streaming();
                self.entries.push(Entry::Error(message));
                self.finish_turn();
            }
        }
        self.transcript_scroll.scroll_to_end();
        self.tokens = estimate_tokens(self);
    }

    fn flush_streaming(&mut self) {
        if !self.streaming.trim().is_empty() {
            self.entries
                .push(Entry::Assistant(std::mem::take(&mut self.streaming)));
        } else {
            self.streaming.clear();
        }
    }

    fn finish_turn(&mut self) {
        self.pending_tool = None;
        self.status = Status::Idle;
        self.step = None;
        self.busy = false;
    }

    fn submit(&mut self, tx: &UnboundedSender<UserCommand>) {
        if self.busy || self.input.trim().is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.input);
        self.cursor = 0;
        self.history_index = None;
        self.history_draft.clear();
        if self.history.last() != Some(&text) {
            self.history.push(text.clone());
            if self.history.len() > 200 {
                self.history.remove(0);
            }
        }
        self.entries.push(Entry::User(text.clone()));
        self.busy = true;
        self.status = Status::Thinking;
        self.transcript_scroll.scroll_to_end();
        let _ = tx.send(UserCommand::Submit(text));
    }

    fn detach_history(&mut self) {
        self.history_index = None;
        self.history_draft.clear();
    }

    fn recall(&mut self, older: bool) {
        if self.history.is_empty() {
            return;
        }
        if older {
            let index = match self.history_index {
                Some(0) => 0,
                Some(index) => index - 1,
                None => {
                    self.history_draft = self.input.clone();
                    self.history.len() - 1
                }
            };
            self.history_index = Some(index);
            self.input = self.history[index].clone();
        } else if let Some(index) = self.history_index {
            if index + 1 < self.history.len() {
                self.history_index = Some(index + 1);
                self.input = self.history[index + 1].clone();
            } else {
                self.history_index = None;
                self.input = std::mem::take(&mut self.history_draft);
            }
        }
        self.cursor = self.input.len();
    }
}

pub async fn run(mut agent: Agent) -> Result<()> {
    let model = agent.model_name().to_owned();
    let session_path = agent.session_path().map(PathBuf::from);
    let initial_tokens = agent.estimated_tokens();
    let cwd = display_cwd()?;
    let initial_tty_size = stty_terminal_size();

    configure_terminal()?;
    defer! { let _ = restore_terminal(); }
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend).context("无法初始化终端")?;
    if let Some((width, height)) = initial_tty_size {
        terminal
            .resize(Rect::new(0, 0, width, height))
            .context("同步终端画布失败")?;
    }
    terminal.clear().context("无法清空终端")?;

    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<UserCommand>();
    let sink = event_tx.clone();
    agent.set_event_sink(Some(Box::new(move |event| {
        let _ = sink.send(event);
    })));
    let agent_handle = tokio::spawn(run_agent(agent, cmd_rx, event_tx));

    let mut app = App::new(model, session_path, cwd);
    app.tokens = initial_tokens;
    let mut input_events = EventStream::new();

    loop {
        while let Ok(event) = event_rx.try_recv() {
            app.apply_agent_event(event);
        }
        app.tick = app.tick.wrapping_add(1);

        // Some embedded terminals update the PTY size without delivering a
        // reliable SIGWINCH/Resize event. Reconcile with `stty size` once per
        // second so the alternate-screen viewport cannot remain letterboxed.
        if app.tick % 20 == 0 {
            if let Some((width, height)) = stty_terminal_size() {
                let size = terminal.size().context("读取终端画布失败")?;
                if size.width != width || size.height != height {
                    terminal
                        .resize(Rect::new(0, 0, width, height))
                        .context("同步终端画布失败")?;
                    terminal.clear().context("重绘终端失败")?;
                }
            }
        }

        terminal.autoresize().context("调整终端画布失败")?;
        terminal
            .draw(|frame| render(frame, &mut app))
            .context("渲染失败")?;

        tokio::select! {
            biased;
            event = input_events.next() => match event {
                Some(Ok(Event::Resize(width, height))) => {
                    terminal.resize(Rect::new(0, 0, width, height)).context("调整终端画布失败")?;
                    terminal.clear().context("重绘终端失败")?;
                }
                Some(Ok(event)) => handle_event(&mut app, event, &cmd_tx)?,
                Some(Err(error)) => return Err(error).context("读取终端事件失败"),
                None => break,
            },
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }

        if app.should_quit {
            break;
        }
    }

    let _ = cmd_tx.send(UserCommand::Shutdown);
    drop(cmd_tx);
    agent_handle.abort();
    Ok(())
}

async fn run_agent(
    mut agent: Agent,
    mut rx: UnboundedReceiver<UserCommand>,
    events: UnboundedSender<AgentEvent>,
) -> Result<()> {
    while let Some(command) = rx.recv().await {
        match command {
            UserCommand::Submit(text) => {
                let stream = events.clone();
                let on_text = move |piece: &str| {
                    let _ = stream.send(AgentEvent::Delta(piece.to_owned()));
                };
                if let Err(error) = agent.run_turn(text, on_text).await {
                    let _ = events.send(AgentEvent::Error(format!("{error:#}")));
                }
            }
            UserCommand::Reset => {
                if let Err(error) = agent.clear_history() {
                    let _ = events.send(AgentEvent::Error(format!("{error:#}")));
                }
            }
            UserCommand::Shutdown => break,
        }
    }
    Ok(())
}

fn handle_event(app: &mut App, event: Event, tx: &UnboundedSender<UserCommand>) -> Result<()> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => handle_key(app, key, tx),
        Event::Paste(text) if !app.busy => {
            app.detach_history();
            let text = text.replace("\r\n", "\n");
            app.input.insert_str(app.cursor, &text);
            app.cursor += text.len();
        }
        Event::Mouse(mouse) => handle_mouse(app, mouse),
        _ => {}
    }
    Ok(())
}

fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::ScrollUp => app.transcript_scroll.scroll_by(-3),
        MouseEventKind::ScrollDown => app.transcript_scroll.scroll_by(3),
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(point) = transcript_point(app, mouse.column, mouse.row) {
                app.selection_anchor = Some(point);
                app.selection_focus = Some(point);
                app.selecting = true;
            } else {
                app.selection_anchor = None;
                app.selection_focus = None;
                app.selecting = false;
            }
        }
        MouseEventKind::Drag(MouseButton::Left) if app.selecting => {
            if let Some(point) = transcript_point_clamped(app, mouse.column, mouse.row) {
                app.selection_focus = Some(point);
            }
        }
        MouseEventKind::Up(MouseButton::Left) if app.selecting => {
            if let Some(point) = transcript_point_clamped(app, mouse.column, mouse.row) {
                app.selection_focus = Some(point);
            }
            app.selecting = false;
            if let Some(text) = selected_text(app) {
                let _ = copy_to_clipboard(&text);
            }
        }
        _ => {}
    }
}

fn handle_key(app: &mut App, key: KeyEvent, tx: &UnboundedSender<UserCommand>) {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.should_quit = true;
        return;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('l') {
        if !app.busy {
            app.entries.clear();
            app.streaming.clear();
            app.transcript_scroll.scroll_to_end();
            app.tokens = 0;
            app.entries.push(Entry::Info("已清空上下文".into()));
            let _ = tx.send(UserCommand::Reset);
        }
        return;
    }
    if app.busy {
        match key.code {
            KeyCode::PageUp => app.transcript_scroll.scroll_by(-8),
            KeyCode::PageDown => app.transcript_scroll.scroll_by(8),
            _ => {}
        }
        return;
    }

    match key.code {
        KeyCode::Enter
            if key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
        {
            insert_text(app, "\n");
        }
        KeyCode::Enter => app.submit(tx),
        KeyCode::Backspace if app.cursor > 0 => {
            app.detach_history();
            let previous = previous_grapheme_boundary(&app.input, app.cursor);
            app.input.drain(previous..app.cursor);
            app.cursor = previous;
        }
        KeyCode::Delete if app.cursor < app.input.len() => {
            app.detach_history();
            let next = next_grapheme_boundary(&app.input, app.cursor);
            app.input.drain(app.cursor..next);
        }
        KeyCode::Left if app.cursor > 0 => {
            app.cursor = previous_grapheme_boundary(&app.input, app.cursor);
        }
        KeyCode::Right if app.cursor < app.input.len() => {
            app.cursor = next_grapheme_boundary(&app.input, app.cursor);
        }
        KeyCode::Home => app.cursor = line_start(&app.input, app.cursor),
        KeyCode::End => app.cursor = line_end(&app.input, app.cursor),
        KeyCode::Up => app.recall(true),
        KeyCode::Down => app.recall(false),
        KeyCode::PageUp => app.transcript_scroll.scroll_by(-8),
        KeyCode::PageDown => app.transcript_scroll.scroll_by(8),
        KeyCode::Tab => insert_text(app, "  "),
        KeyCode::Char('d')
            if key.modifiers.contains(KeyModifiers::CONTROL) && app.input.is_empty() =>
        {
            app.should_quit = true;
        }
        KeyCode::Esc if app.input.is_empty() => app.should_quit = true,
        KeyCode::Esc => {
            app.input.clear();
            app.cursor = 0;
            app.detach_history();
        }
        KeyCode::Char(ch)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            let mut encoded = [0; 4];
            insert_text(app, ch.encode_utf8(&mut encoded));
        }
        _ => {}
    }
}

fn insert_text(app: &mut App, text: &str) {
    app.detach_history();
    app.input.insert_str(app.cursor, text);
    app.cursor += text.len();
}

fn render(frame: &mut ratatui::Frame<'_>, app: &mut App) {
    let area = frame.area();
    let editor_rows = editor_rows(&app.input, area.width.max(1) as usize)
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

fn render_conversation(frame: &mut ratatui::Frame<'_>, app: &mut App, area: Rect) {
    let width = area.width.max(1) as usize;
    let mut lines = Vec::new();
    if app.entries.is_empty() && app.streaming.is_empty() {
        lines.extend(render_welcome(
            &app.model,
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
    frame.render_widget(
        Paragraph::new(Text::from(visible)).wrap(Wrap { trim: false }),
        area,
    );
    render_selection(frame, app);
}

fn render_welcome(model: &str, philosophy: Philosophy, width: usize) -> Vec<Line<'static>> {
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

fn render_philosophy(philosophy: Philosophy, width: usize) -> Vec<Line<'static>> {
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

fn philosophy_box_line(text: &str, width: usize, style: Style) -> Line<'static> {
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

fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn transcript_point(app: &App, column: u16, row: u16) -> Option<SelectionPoint> {
    let area = app.transcript_area;
    if column < area.x || column >= area.right() || row < area.y || row >= area.bottom() {
        return None;
    }
    transcript_point_clamped(app, column, row)
}

fn transcript_point_clamped(app: &App, column: u16, row: u16) -> Option<SelectionPoint> {
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

fn selection_bounds(app: &App) -> Option<(SelectionPoint, SelectionPoint)> {
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

fn grapheme_cell_range(text: &str, column: usize) -> Option<(usize, usize)> {
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

fn selection_columns(
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

fn render_selection(frame: &mut ratatui::Frame<'_>, app: &App) {
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

fn selected_text(app: &App) -> Option<String> {
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

fn slice_by_columns(text: &str, from: usize, to: usize) -> String {
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

fn copy_to_clipboard(text: &str) -> bool {
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

fn render_status(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
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

fn render_entry(entry: &Entry, width: usize) -> Vec<Line<'static>> {
    match entry {
        Entry::User(text) => {
            let inner = width.saturating_sub(4).max(1);
            let background = Style::default().bg(USER_BG).fg(Color::White);
            let mut lines = vec![Line::from(Span::styled(" ".repeat(width), background))];
            for text_line in text.lines().flat_map(|line| wrap_text(line, inner)) {
                let padding = inner.saturating_sub(UnicodeWidthStr::width(text_line.as_str()));
                lines.push(Line::from(Span::styled(
                    format!("  {text_line}{}  ", " ".repeat(padding)),
                    background,
                )));
            }
            lines.push(Line::from(Span::styled(" ".repeat(width), background)));
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

fn render_tool(name: &str, args: &str, result: Option<&str>, width: usize) -> Vec<Line<'static>> {
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

fn render_editor(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let border_color = if app.busy { MUTED } else { ACCENT };
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(border_color));
    let placeholder = if app.input.is_empty() && !app.busy {
        "输入消息"
    } else {
        ""
    };
    let content = if placeholder.is_empty() {
        app.input.as_str()
    } else {
        placeholder
    };
    let style = if placeholder.is_empty() {
        Style::default()
    } else {
        Style::default().fg(MUTED)
    };
    let paragraph = Paragraph::new(content)
        .style(style)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);

    if !app.busy {
        let width = area.width.max(1) as usize;
        let (column, row) = cursor_position(&app.input, app.cursor, width.saturating_sub(1));
        let x = area.x + column as u16;
        let y = area.y + 1 + row as u16;
        if x < area.right() && y < area.bottom() {
            frame.set_cursor_position((x, y));
        }
    }
}

fn render_footer(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let session = app
        .session_path
        .as_ref()
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("临时会话");
    let step = app
        .step
        .map(|(current, total)| format!(" · 步骤 {current}/{total}"))
        .unwrap_or_default();
    let left = format!("{} · {session}", app.cwd);
    let right = format!("约 {} 词元{step} · {}", app.tokens, app.status.label());
    let first = join_sides(&left, &right, area.width as usize);
    let hints = if app.busy {
        "Ctrl-C 退出 · PgUp/PgDn 滚动"
    } else {
        "Enter 发送 · Shift-Enter 换行 · ↑/↓ 历史 · Ctrl-L 清空 · Ctrl-D 退出"
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

fn pad_lines(lines: Vec<Line<'static>>, padding: usize) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|line| {
            let mut spans = vec![Span::raw(" ".repeat(padding))];
            spans.extend(line.spans);
            Line::from(spans)
        })
        .collect()
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
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

fn visible_assistant_text(text: &str) -> String {
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

fn editor_rows(text: &str, width: usize) -> usize {
    cursor_position(text, text.len(), width).1 + 1
}

fn cursor_position(text: &str, cursor: usize, width: usize) -> (usize, usize) {
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

fn previous_grapheme_boundary(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .grapheme_indices(true)
        .next_back()
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn next_grapheme_boundary(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .graphemes(true)
        .next()
        .map(|value| cursor + value.len())
        .unwrap_or(cursor)
}

fn line_start(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0)
}

fn line_end(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .find('\n')
        .map(|index| cursor + index)
        .unwrap_or(text.len())
}

fn tool_summary(args: &str) -> String {
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

fn truncate_to_width(text: &str, width: usize) -> String {
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

fn join_sides(left: &str, right: &str, width: usize) -> String {
    let right = truncate_to_width(right, width / 2);
    let right_width = UnicodeWidthStr::width(right.as_str());
    let left = truncate_to_width(left, width.saturating_sub(right_width + 2));
    let padding = width.saturating_sub(UnicodeWidthStr::width(left.as_str()) + right_width);
    format!("{left}{}{right}", " ".repeat(padding))
}

fn estimate_tokens(app: &App) -> usize {
    let chars = app
        .entries
        .iter()
        .map(|entry| match entry {
            Entry::User(text) | Entry::Assistant(text) | Entry::Info(text) | Entry::Error(text) => {
                text.chars().count()
            }
            Entry::Tool { args, result, .. } => {
                args.chars().count() + result.as_deref().unwrap_or("").chars().count()
            }
        })
        .sum::<usize>()
        + app.streaming.chars().count();
    chars.div_ceil(4)
}

fn display_cwd() -> Result<String> {
    let cwd = env::current_dir()?.display().to_string();
    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        let home = home.display().to_string();
        if cwd == home {
            return Ok("~".into());
        }
        if let Some(rest) = cwd.strip_prefix(&(home + "/")) {
            return Ok(format!("~/{rest}"));
        }
    }
    Ok(cwd)
}

fn stty_terminal_size() -> Option<(u16, u16)> {
    let output = Command::new("stty").arg("size").output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse_terminal_size(std::str::from_utf8(&output.stdout).ok()?)
}

fn parse_terminal_size(value: &str) -> Option<(u16, u16)> {
    let mut parts = value.split_whitespace();
    let rows = parts.next()?.parse().ok()?;
    let columns = parts.next()?.parse().ok()?;
    if rows == 0 || columns == 0 || parts.next().is_some() {
        return None;
    }
    Some((columns, rows))
}

fn configure_terminal() -> Result<()> {
    enable_raw_mode().context("无法进入终端原始模式")?;
    execute!(
        stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )
    .context("无法进入终端备用屏幕")?;
    Ok(())
}

fn restore_terminal() -> Result<()> {
    let _ = execute!(
        stdout(),
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    );
    let _ = disable_raw_mode();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel() -> UnboundedSender<UserCommand> {
        mpsc::unbounded_channel().0
    }

    #[test]
    fn unicode_editing_uses_complete_graphemes() {
        let mut app = App::new("model".into(), None, "~".into());
        app.input = "a👨‍👩‍👧‍👦中".into();
        app.cursor = app.input.len();
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
            &channel(),
        );
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            &channel(),
        );
        assert_eq!(app.input, "a中");
        assert_eq!(&app.input[app.cursor..], "中");
    }

    #[test]
    fn history_restores_the_unsent_draft() {
        let mut app = App::new("model".into(), None, "~".into());
        app.history = vec!["first".into(), "second".into()];
        app.input = "draft".into();
        app.cursor = app.input.len();
        app.recall(true);
        assert_eq!(app.input, "second");
        app.recall(true);
        assert_eq!(app.input, "first");
        app.recall(false);
        app.recall(false);
        assert_eq!(app.input, "draft");
    }

    #[test]
    fn mouse_wheel_scrolls_transcript_without_recalling_prompt_history() {
        let mut app = App::new("model".into(), None, "~".into());
        app.history = vec!["上一条提示".into()];
        app.input = "当前草稿".into();
        app.cursor = app.input.len();
        app.transcript_area = Rect::new(0, 0, 80, 20);
        app.transcript_lines = (0..60).map(|row| format!("第 {row} 行")).collect();
        app.transcript_scroll.update_layout(60, 20);

        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 10,
                row: 5,
                modifiers: KeyModifiers::NONE,
            },
        );

        assert_eq!(app.transcript_scroll.top, 37);
        assert_eq!(app.input, "当前草稿");
        assert_eq!(app.history_index, None);
    }

    #[test]
    fn transcript_selection_preserves_unicode_text() {
        let mut app = App::new("model".into(), None, "~".into());
        app.transcript_lines = vec!["ab中文".into(), "second".into()];
        app.selection_anchor = Some(SelectionPoint { row: 0, column: 2 });
        app.selection_focus = Some(SelectionPoint { row: 1, column: 2 });
        assert_eq!(selected_text(&app).as_deref(), Some("中文\nsec"));
    }

    #[test]
    fn cursor_position_respects_cjk_newlines_and_wrap() {
        assert_eq!(cursor_position("中文", "中文".len(), 10), (4, 0));
        assert_eq!(cursor_position("a\n中", "a\n中".len(), 10), (2, 1));
        assert_eq!(cursor_position("1234567890", 10, 10), (0, 1));
    }

    #[test]
    fn agent_events_form_a_conversation() {
        let mut app = App::new("model".into(), None, "~".into());
        app.busy = true;
        app.apply_agent_event(AgentEvent::Delta("hello".into()));
        app.apply_agent_event(AgentEvent::ToolStart {
            name: "bash".into(),
            args: "{}".into(),
        });
        app.apply_agent_event(AgentEvent::ToolResult {
            output: "ok".into(),
        });
        app.apply_agent_event(AgentEvent::Complete);
        assert!(!app.busy);
        assert!(matches!(app.entries[0], Entry::Assistant(_)));
        assert!(matches!(app.entries[1], Entry::Tool { .. }));
    }

    #[test]
    fn thinking_blocks_are_not_rendered_as_assistant_text() {
        let text = "<think>private reasoning\nmore</think>\n你好";
        assert_eq!(visible_assistant_text(text), "你好");
        assert_eq!(visible_assistant_text("<think>still streaming"), "");
    }

    #[test]
    fn layout_renders_at_the_full_backend_size() {
        use ratatui::backend::TestBackend;
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new("MiniMax-M3".into(), None, "~/code".into());
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert_eq!(terminal.size().unwrap(), Rect::new(0, 0, 120, 40).into());
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.area, Rect::new(0, 0, 120, 40));
    }

    #[test]
    fn welcome_banner_is_branded_and_responsive() {
        let wide = render_welcome("MiniMax-M3", PHILOSOPHIES[0], 120)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(wide.contains("██████╗ ██╗ ██████╗ ██████╗"));
        assert!(wide.contains("使用 Rust 构建，追求极致性能。"));
        assert!(wide.contains("原生终端，毫秒必争。"));
        assert!(wide.contains("编程哲学"));
        assert!(wide.contains("DRY (Don't Repeat Yourself)"));
        assert!(wide.contains("消除知识重复，让每条规则只有一个权威来源。"));
        assert!(wide.contains("MiniMax-M3"));

        let compact = render_welcome("MiniMax-M3", PHILOSOPHIES[0], 40)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(compact.contains("rico v"));
        assert!(!compact.contains("██████╗"));
        assert!(compact.contains("使用 Rust 构建，追求极致性能。"));
        assert!(compact.contains("哲思 · DRY"));
    }

    #[test]
    fn visible_interface_is_localized_in_chinese() {
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(140, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new("MiniMax-M3".into(), None, "~/code".into());
        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        let screen = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        let compact_screen = screen.replace(' ', "");

        for expected in [
            "模型 MiniMax-M3",
            "使用 Rust 构建，追求极致性能。",
            "编程哲学",
            "输入任务开始对话。",
            "输入消息",
            "临时会话",
            "词元",
            "就绪",
            "Enter 发送",
            "Shift-Enter 换行",
            "↑/↓ 历史",
            "Ctrl-L 清空",
            "Ctrl-D 退出",
        ] {
            let compact_expected = expected.replace(' ', "");
            assert!(
                compact_screen.contains(&compact_expected),
                "界面缺少中文文案：{expected}\n{screen}"
            );
        }

        for obsolete in [
            "Model:",
            "Enter a prompt to begin.",
            "Type a message",
            "ephemeral",
            "tokens",
            "ready",
            "Enter send",
        ] {
            assert!(!screen.contains(obsolete), "界面仍包含英文文案：{obsolete}");
        }

        assert_eq!(Status::Thinking.label(), "正在思考…");
        assert_eq!(Status::RunningTool("bash".into()).label(), "正在运行 bash…");
        assert_eq!(Status::Compacting.label(), "正在压缩上下文…");
    }

    #[test]
    fn parses_stty_rows_then_columns() {
        assert_eq!(parse_terminal_size("52 183\n"), Some((183, 52)));
        assert_eq!(parse_terminal_size("0 183"), None);
        assert_eq!(parse_terminal_size("invalid"), None);
    }
}
