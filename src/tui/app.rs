//! Core TUI state, session tracking, and user commands.

use std::{
    env,
    path::PathBuf,
    process,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use ratatui::layout::Rect;
use tokio::sync::mpsc::UnboundedSender;

use crate::{agent::AgentEvent, session::CacheStats};

#[derive(Debug, Clone, Copy)]
pub struct Philosophy {
    pub principle: &'static str,
    pub original: &'static str,
    pub meaning: &'static str,
}

pub const PHILOSOPHIES: &[Philosophy] = &[
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

pub fn select_philosophy_index() -> usize {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as usize;
    (time ^ process::id() as usize).wrapping_mul(0x9e37_79b1) % PHILOSOPHIES.len()
}

#[derive(Debug)]
pub enum UserCommand {
    Submit(String),
    SwitchProvider(Option<String>),
    LoginProvider { provider: String, key: String },
    Reset,
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum Entry {
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
pub enum Status {
    Idle,
    Thinking,
    EnteringApiKey,
    SavingCredentials,
    SwitchingProvider,
    RunningTool(String),
    Compacting,
}

impl Status {
    pub fn label(&self) -> String {
        match self {
            Self::Idle => "就绪".into(),
            Self::Thinking => "正在思考…".into(),
            Self::EnteringApiKey => "请输入 API Key…".into(),
            Self::SavingCredentials => "正在保存认证信息…".into(),
            Self::SwitchingProvider => "正在切换 provider…".into(),
            Self::RunningTool(name) => format!("正在运行 {name}…"),
            Self::Compacting => "正在压缩上下文…".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionPoint {
    pub row: usize,
    pub column: usize,
}

#[derive(Default)]
pub struct ScrollViewState {
    pub top: usize,
    pub content_height: usize,
    pub viewport_height: usize,
    pub follow_end: bool,
}

impl ScrollViewState {
    pub fn following_end() -> Self {
        Self {
            follow_end: true,
            ..Self::default()
        }
    }

    pub fn update_layout(&mut self, content_height: usize, viewport_height: usize) {
        self.content_height = content_height;
        self.viewport_height = viewport_height;
        let end = content_height.saturating_sub(viewport_height);
        self.top = if self.follow_end {
            end
        } else {
            self.top.min(end)
        };
    }

    pub fn scroll_by(&mut self, delta: i32) {
        let end = self.content_height.saturating_sub(self.viewport_height);
        self.top = (self.top as i64 + delta as i64).clamp(0, end as i64) as usize;
        self.follow_end = self.top == end;
    }

    pub fn scroll_to_end(&mut self) {
        self.follow_end = true;
        self.top = self.content_height.saturating_sub(self.viewport_height);
    }
}

pub struct App {
    pub entries: Vec<Entry>,
    pub streaming: String,
    pub pending_tool: Option<(String, String)>,
    pub input: String,
    pub cursor: usize,
    pub history: Vec<String>,
    pub history_index: Option<usize>,
    pub history_draft: String,
    pub transcript_scroll: ScrollViewState,
    pub transcript_area: Rect,
    pub transcript_lines: Vec<String>,
    pub selection_anchor: Option<SelectionPoint>,
    pub selection_focus: Option<SelectionPoint>,
    pub selecting: bool,
    pub login_provider: Option<String>,
    pub philosophy_index: usize,
    pub provider: String,
    pub providers: Vec<String>,
    pub model: String,
    pub cwd: String,
    pub session_path: Option<PathBuf>,
    pub tokens: usize,
    pub cache_stats: CacheStats,
    pub status: Status,
    pub step: Option<(usize, Option<usize>)>,
    pub busy: bool,
    pub should_quit: bool,
    pub tick: usize,
}

impl App {
    pub fn new(model: String, session_path: Option<PathBuf>, cwd: String) -> Self {
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
            login_provider: None,
            philosophy_index: select_philosophy_index(),
            provider: String::new(),
            providers: Vec::new(),
            model,
            cwd,
            session_path,
            tokens: 0,
            cache_stats: CacheStats::default(),
            status: Status::Idle,
            step: None,
            busy: false,
            should_quit: false,
            tick: 0,
        }
    }

    pub fn apply_agent_event(&mut self, event: AgentEvent) {
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
            AgentEvent::Usage(usage) => {
                self.cache_stats.record_usage(&usage);
            }
            AgentEvent::ProviderChanged { provider, model } => {
                if !self.providers.iter().any(|name| name == &provider) {
                    self.providers.push(provider.clone());
                }
                self.provider = provider;
                self.model = model;
                self.entries.push(Entry::Info(format!(
                    "已切换到 {}（模型 {}）",
                    self.provider, self.model
                )));
                self.finish_turn();
            }
        }
        self.transcript_scroll.scroll_to_end();
        self.tokens = estimate_tokens(self);
    }

    pub fn flush_streaming(&mut self) {
        if !self.streaming.trim().is_empty() {
            self.entries
                .push(Entry::Assistant(std::mem::take(&mut self.streaming)));
        } else {
            self.streaming.clear();
        }
    }

    pub fn finish_turn(&mut self) {
        self.pending_tool = None;
        self.status = Status::Idle;
        self.step = None;
        self.busy = false;
    }

    pub fn submit(&mut self, tx: &UnboundedSender<UserCommand>) {
        if self.busy || self.input.trim().is_empty() {
            return;
        }
        let trimmed = self.input.trim();
        if let Some(provider) = self.login_provider.take() {
            let key = std::mem::take(&mut self.input);
            self.cursor = 0;
            self.detach_history();
            if key.trim().is_empty() {
                self.login_provider = Some(provider);
                self.status = Status::EnteringApiKey;
                return;
            }
            self.busy = true;
            self.status = Status::SavingCredentials;
            self.transcript_scroll.scroll_to_end();
            let _ = tx.send(UserCommand::LoginProvider { provider, key });
            return;
        }
        if trimmed == "/login" || trimmed.starts_with("/login ") {
            let requested = trimmed
                .strip_prefix("/login")
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_ascii_lowercase);
            let Some(provider) = requested else {
                self.entries.push(Entry::Info(
                    "用法：/login minimax 或 /login 9router，然后在输入框中粘贴 API Key。\nAPI Key 将以掩码显示，不会写入会话历史。".into(),
                ));
                self.transcript_scroll.scroll_to_end();
                return;
            };
            if !matches!(provider.as_str(), "minimax" | "9router") {
                self.entries.push(Entry::Error(
                    "不支持的 provider，可选：minimax、9router".into(),
                ));
                self.transcript_scroll.scroll_to_end();
                return;
            }
            self.input.clear();
            self.cursor = 0;
            self.login_provider = Some(provider.clone());
            self.status = Status::EnteringApiKey;
            self.entries.push(Entry::Info(format!(
                "正在登录 {provider}，请在下方输入 API Key（输入内容会被掩码）。"
            )));
            self.transcript_scroll.scroll_to_end();
            return;
        }
        if trimmed == "/providers" {
            let text = std::mem::take(&mut self.input);
            self.cursor = 0;
            self.detach_history();
            self.entries.push(Entry::User(text));
            let available = self
                .providers
                .iter()
                .map(|name| {
                    if name == &self.provider {
                        format!("• {name}（当前）")
                    } else {
                        format!("• {name}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            self.entries.push(Entry::Info(format!(
                "当前 provider: {}\n当前模型: {}\n\n可用 provider:\n{}",
                self.provider, self.model, available
            )));
            self.transcript_scroll.scroll_to_end();
            return;
        }
        if trimmed == "/provider" || trimmed.starts_with("/provider ") {
            let requested = trimmed
                .strip_prefix("/provider")
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned);
            let text = std::mem::take(&mut self.input);
            self.cursor = 0;
            self.detach_history();
            self.entries.push(Entry::User(text));
            self.busy = true;
            self.status = Status::SwitchingProvider;
            self.transcript_scroll.scroll_to_end();
            let _ = tx.send(UserCommand::SwitchProvider(requested));
            return;
        }
        if trimmed == "/cache" || trimmed == "/stats" {
            let text = std::mem::take(&mut self.input);
            self.cursor = 0;
            self.detach_history();
            self.entries.push(Entry::User(text));
            self.entries
                .push(Entry::Info(self.cache_stats.summary_text()));
            self.transcript_scroll.scroll_to_end();
            return;
        }
        if trimmed == "/session" {
            let text = std::mem::take(&mut self.input);
            self.cursor = 0;
            self.detach_history();
            self.entries.push(Entry::User(text));
            let path_str = self
                .session_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "当前为临时会话".to_string());
            let mut info = format!("会话文件: {path_str}");
            if self.cache_stats.requests_count > 0 {
                info.push_str("\n\n");
                info.push_str(&self.cache_stats.summary_text());
            }
            self.entries.push(Entry::Info(info));
            self.transcript_scroll.scroll_to_end();
            return;
        }
        if trimmed == "/clear" {
            self.input.clear();
            self.cursor = 0;
            self.detach_history();
            self.entries.clear();
            self.cache_stats = CacheStats::default();
            self.entries.push(Entry::Info("对话历史已清空。".into()));
            self.transcript_scroll.scroll_to_end();
            let _ = tx.send(UserCommand::Reset);
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

    pub fn detach_history(&mut self) {
        self.history_index = None;
        self.history_draft.clear();
    }

    pub fn recall(&mut self, older: bool) {
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

    pub fn insert_text(&mut self, text: &str) {
        self.detach_history();
        self.input.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    pub fn cancel_login(&mut self) {
        self.login_provider = None;
        self.input.clear();
        self.cursor = 0;
        self.detach_history();
        self.status = Status::Idle;
    }
}

pub fn estimate_tokens(app: &App) -> usize {
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

pub fn display_cwd() -> Result<String> {
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
