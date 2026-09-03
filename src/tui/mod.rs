//! Pi-inspired terminal interface for rico.
//!
//! The UI is deliberately quiet: conversation first, a bordered editor near
//! the bottom, and two compact footer lines. All editing uses Unicode grapheme
//! boundaries, while cursor placement uses terminal cell widths.

pub mod app;
pub mod editor;
pub mod event;
pub mod selection;
pub mod view;

use std::{io::stdout, path::PathBuf, process::Command, time::Duration};

use anyhow::{Context, Result};
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::StreamExt;
use ratatui::{backend::CrosstermBackend, layout::Rect, Terminal};
use scopeguard::defer;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::agent::{Agent, AgentEvent};

use self::{
    app::{App, UserCommand},
    event::handle_event,
    view::render,
};

pub async fn run(mut agent: Agent) -> Result<()> {
    let provider = agent.provider_name().to_owned();
    let providers = agent.provider_names();
    let model = agent.model_name().to_owned();
    let session_path = agent.session_path().map(PathBuf::from);
    let initial_tokens = agent.estimated_tokens();
    let initial_cache_stats = agent.cache_stats();
    let cwd = app::display_cwd()?;
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
    app.provider = provider;
    app.providers = providers;
    app.tokens = initial_tokens;
    app.cache_stats = initial_cache_stats;
    let mut input_events = EventStream::new();

    loop {
        while let Ok(event) = event_rx.try_recv() {
            app.apply_agent_event(event);
        }
        app.tick = app.tick.wrapping_add(1);

        // Some embedded terminals update the PTY size without delivering a
        // reliable SIGWINCH/Resize event. Reconcile with `stty size` once per
        // second so the alternate-screen viewport cannot remain letterboxed.
        if app.tick.is_multiple_of(20) {
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
            UserCommand::SwitchProvider(requested) => {
                match agent.switch_provider(requested.as_deref()) {
                    Ok((provider, model)) => {
                        let _ = events.send(AgentEvent::ProviderChanged { provider, model });
                    }
                    Err(error) => {
                        let _ = events.send(AgentEvent::Error(format!("{error:#}")));
                    }
                }
            }
            UserCommand::LoginProvider { provider, key } => {
                match agent.login_provider(&provider, key) {
                    Ok((provider, model)) => {
                        let _ = events.send(AgentEvent::ProviderChanged { provider, model });
                    }
                    Err(error) => {
                        let _ = events.send(AgentEvent::Error(format!("登录失败：{error:#}")));
                    }
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
    use super::{
        app::{Entry, SelectionPoint, Status, PHILOSOPHIES},
        editor::{cursor_position, wrap_text},
        event::{handle_key, handle_mouse},
        selection::selected_text,
        view::{line_text, render, render_welcome, visible_assistant_text},
        *,
    };
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

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
    fn word_wrapping_and_kinsoku_punctuation() {
        let text = "Master-Detail (Viewport / Markdown Widget)";
        let wrapped = wrap_text(text, 25);
        assert_eq!(wrapped[0], "Master-Detail (Viewport /");
        assert_eq!(wrapped[1], "Markdown Widget)");

        let cjk_punct = "这是一段很长的文字，用于测试标点符号。";
        let wrapped_cjk = wrap_text(cjk_punct, 18);
        assert!(!wrapped_cjk[1].starts_with('，'));
        assert!(!wrapped_cjk[1].starts_with('。'));
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

    #[test]
    fn cache_slash_command_shows_stats_in_tui() {
        use crate::provider::TokenUsage;

        let mut app = App::new("MiniMax-M3".into(), None, "~/code".into());
        app.cache_stats.record_usage(&TokenUsage {
            prompt_tokens: 1000,
            completion_tokens: 100,
            total_tokens: 1100,
            cached_tokens: 750,
        });

        app.input = "/cache".into();
        let (tx, mut rx) = mpsc::unbounded_channel::<UserCommand>();
        app.submit(&tx);

        assert_eq!(app.input, "");
        assert!(rx.try_recv().is_err(), "本地指令不应向 agent 发送命令");
        assert_eq!(app.entries.len(), 2);
        assert!(matches!(&app.entries[0], Entry::User(text) if text == "/cache"));
        if let Entry::Info(info) = &app.entries[1] {
            assert!(info.contains("Prompt 缓存命中统计"));
            assert!(info.contains("75.0%"));
            assert!(info.contains("750"));
        } else {
            panic!("第二条 entry 应为 Info");
        }
    }

    #[test]
    fn provider_commands_list_and_switch_configured_providers() {
        let mut app = App::new("MiniMax-M3".into(), None, "~/code".into());
        app.provider = "minimax".into();
        app.providers = vec!["minimax".into(), "9router".into()];
        let (tx, mut rx) = mpsc::unbounded_channel::<UserCommand>();

        app.input = "/providers".into();
        app.submit(&tx);
        assert!(matches!(
            app.entries.last(),
            Some(Entry::Info(info)) if info.contains("minimax（当前）") && info.contains("9router")
        ));

        app.input = "/provider 9router".into();
        app.submit(&tx);
        assert!(app.busy);
        assert_eq!(app.status, Status::SwitchingProvider);
        assert!(matches!(
            rx.try_recv(),
            Ok(UserCommand::SwitchProvider(Some(name))) if name == "9router"
        ));

        app.apply_agent_event(AgentEvent::ProviderChanged {
            provider: "9router".into(),
            model: "kr/claude-sonnet-4.5".into(),
        });
        assert_eq!(app.provider, "9router");
        assert_eq!(app.model, "kr/claude-sonnet-4.5");
        assert!(!app.busy);
    }

    #[test]
    fn login_command_masks_key_and_does_not_add_it_to_history() {
        let mut app = App::new("MiniMax-M3".into(), None, "~/code".into());
        let (tx, mut rx) = mpsc::unbounded_channel::<UserCommand>();

        app.input = "/login 9router".into();
        app.submit(&tx);
        assert_eq!(app.login_provider.as_deref(), Some("9router"));
        assert!(app
            .entries
            .iter()
            .all(|entry| { !matches!(entry, Entry::User(text) if text.contains("9router-key")) }));

        app.input = "9router-secret".into();
        app.cursor = app.input.len();
        app.submit(&tx);
        assert!(app.busy);
        assert!(matches!(
            rx.try_recv(),
            Ok(UserCommand::LoginProvider { provider, key })
                if provider == "9router" && key == "9router-secret"
        ));
        assert!(app.entries.iter().all(|entry| {
            !matches!(entry, Entry::User(text) | Entry::Info(text) if text.contains("9router-secret"))
        }));
    }

    #[test]
    fn cache_stats_displayed_in_footer() {
        use crate::provider::TokenUsage;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(140, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new("MiniMax-M3".into(), None, "~/code".into());
        app.cache_stats.record_usage(&TokenUsage {
            prompt_tokens: 1000,
            completion_tokens: 100,
            total_tokens: 1100,
            cached_tokens: 800,
        });

        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        let compact = screen.replace(' ', "");
        assert!(compact.contains("缓存命中80.0%"));
        assert!(compact.contains("800/1000"));
    }
}
