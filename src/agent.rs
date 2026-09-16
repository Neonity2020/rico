use std::{
    env,
    future::Future,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::{
    auth::{auth_path, AuthStore},
    provider::{Message, OpenAiProvider, TokenUsage},
    session::{CacheStats, SessionStore},
    tools::ToolRegistry,
};

const SYSTEM_PROMPT: &str = r#"你是一个最小但可靠的 coding agent。你的工作目录就是项目根目录。
先检查相关文件，再做修改；修改后运行合适的检查或测试。优先使用 read/edit/write 操作文件，使用 web_search 进行联网信息检索（基于 Exa），使用 bash 执行命令、本地搜索和必要的构建测试。
工具失败时，阅读错误并尝试安全的替代方案。完成后简洁说明改了什么以及验证结果。"#;

/// Hook events emitted by [`Agent`] during a turn. The TUI subscribes to these
/// to drive its interface; the REPL ignores them.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    Delta(String),
    ToolStart {
        name: String,
        args: String,
    },
    ToolResult {
        output: String,
    },
    Step {
        current: usize,
        total: Option<usize>,
    },
    Compacting,
    Complete,
    Cancelled {
        cache_stats: CacheStats,
    },
    Error(String),
    Usage(TokenUsage),
    ProviderChanged {
        provider: String,
        model: String,
    },
}

#[derive(Debug)]
pub struct TurnCancelled;

impl std::fmt::Display for TurnCancelled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("任务已取消")
    }
}

impl std::error::Error for TurnCancelled {}

pub struct Agent {
    providers: Vec<OpenAiProvider>,
    active_provider: Option<usize>,
    tools: ToolRegistry,
    max_steps: Option<usize>,
    messages: Vec<Message>,
    session: SessionStore,
    compact_threshold: usize,
    keep_recent_tokens: usize,
    event_sink: Option<Box<dyn FnMut(AgentEvent) + Send>>,
    cancel_requested: Arc<AtomicBool>,
}

impl Agent {
    pub fn new_with_providers(
        providers: Vec<OpenAiProvider>,
        active_provider: Option<usize>,
        workspace: PathBuf,
        max_steps: Option<usize>,
        resume: bool,
        exa_api_key: Option<String>,
    ) -> Result<Self> {
        if active_provider.is_some_and(|index| index >= providers.len()) {
            anyhow::bail!("当前 provider 索引无效");
        }
        let system = Message::text("system", SYSTEM_PROMPT);
        let (mut session, mut messages) = SessionStore::open(&workspace, resume, system.clone())?;
        if messages.is_empty() {
            messages.push(system.clone());
            session.append_message(&system)?;
        }
        Ok(Self {
            providers,
            active_provider,
            tools: ToolRegistry::coding_tools(workspace, exa_api_key),
            max_steps,
            messages,
            session,
            compact_threshold: env_usize("RICO_COMPACT_TOKENS", 200_000)?,
            keep_recent_tokens: env_usize("RICO_KEEP_RECENT_TOKENS", 20_000)?,
            event_sink: None,
            cancel_requested: Arc::new(AtomicBool::new(false)),
        })
    }

    #[cfg(test)]
    fn new_ephemeral(
        provider: OpenAiProvider,
        workspace: PathBuf,
        max_steps: Option<usize>,
    ) -> Self {
        Self {
            providers: vec![provider],
            active_provider: Some(0),
            tools: ToolRegistry::coding_tools(workspace, None),
            max_steps,
            messages: vec![Message::text("system", SYSTEM_PROMPT)],
            session: SessionStore::ephemeral(),
            compact_threshold: 200_000,
            keep_recent_tokens: 20_000,
            event_sink: None,
            cancel_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn set_event_sink(&mut self, sink: Option<Box<dyn FnMut(AgentEvent) + Send>>) {
        self.event_sink = sink;
    }

    pub fn cancellation_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel_requested)
    }

    pub fn clear_history(&mut self) -> Result<()> {
        let system = Message::text("system", SYSTEM_PROMPT);
        self.messages = vec![system.clone()];
        self.session.append_reset()?;
        self.session.append_message(&system)
    }

    pub fn session_path(&self) -> Option<&std::path::Path> {
        self.session.path()
    }

    pub fn cache_stats(&self) -> CacheStats {
        self.session.cache_stats()
    }

    pub fn estimated_tokens(&self) -> usize {
        estimate_tokens(&self.messages)
    }

    pub fn model_name(&self) -> &str {
        self.provider().map_or("未登录", OpenAiProvider::model_name)
    }

    pub fn provider_name(&self) -> &str {
        self.provider().map_or("未登录", OpenAiProvider::name)
    }

    pub fn provider_names(&self) -> Vec<String> {
        self.providers
            .iter()
            .map(|provider| provider.name().to_owned())
            .collect()
    }

    pub fn switch_provider(&mut self, requested: Option<&str>) -> Result<(String, String)> {
        let next = match requested {
            Some(name) => self
                .providers
                .iter()
                .position(|provider| provider.name().eq_ignore_ascii_case(name))
                .with_context(|| format!("provider 未配置或不存在: {name}"))?,
            None => {
                if self.providers.is_empty() {
                    anyhow::bail!(
                        "尚未登录任何 provider，请先使用 /login minimax、/login 9router 或 /login agnes"
                    );
                }
                self.active_provider
                    .map_or(0, |index| (index + 1) % self.providers.len())
            }
        };
        self.active_provider = Some(next);
        Ok((
            self.provider_name().to_owned(),
            self.model_name().to_owned(),
        ))
    }

    pub fn login_provider(&mut self, requested: &str, key: String) -> Result<(String, String)> {
        let name = match requested.to_ascii_lowercase().as_str() {
            "minimax" => "minimax",
            "9router" | "router" => "9router",
            "agnes" => "agnes",
            _ => anyhow::bail!("不支持的 provider：{requested}（可选 minimax、9router 或 agnes）"),
        };
        let mut auth = AuthStore::load(auth_path()?)?;
        auth.set_api_key(name.to_owned(), key.clone())?;

        let (base_url, model) = match name {
            "minimax" => (
                env::var("MINIMAX_BASE_URL")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| "https://api.minimaxi.com/v1".into()),
                env::var("MINIMAX_MODEL")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| "MiniMax-M3".into()),
            ),
            "9router" => (
                env::var("ROUTER_BASE_URL")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| "http://localhost:20128/v1".into()),
                env::var("ROUTER_MODEL")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| "kr/claude-sonnet-4.5".into()),
            ),
            "agnes" => (
                env::var("AGNES_BASE_URL")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| "https://apihub.agnes-ai.com/v1".into()),
                env::var("AGNES_MODEL")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| "agnes-3.0-flash".into()),
            ),
            _ => unreachable!(),
        };
        let provider = OpenAiProvider::named(name, key, base_url, model);
        if let Some(index) = self
            .providers
            .iter()
            .position(|existing| existing.name() == name)
        {
            self.providers[index] = provider;
            self.active_provider = Some(index);
        } else {
            self.providers.push(provider);
            self.active_provider = Some(self.providers.len() - 1);
        }
        Ok((
            self.provider_name().to_owned(),
            self.model_name().to_owned(),
        ))
    }

    pub async fn run_turn(
        &mut self,
        task: String,
        mut on_text: impl FnMut(&str),
    ) -> Result<String> {
        let checkpoint_messages = self.messages.clone();
        let checkpoint_stats = self.session.cache_stats();
        let result = self.run_turn_active(task, &mut on_text).await;
        let was_cancelled = result
            .as_ref()
            .is_err_and(|error| error.downcast_ref::<TurnCancelled>().is_some());
        self.cancel_requested.store(false, Ordering::Release);
        if was_cancelled {
            self.messages = checkpoint_messages;
            self.session
                .rollback_to(&self.messages, checkpoint_stats)
                .context("取消任务后回滚会话失败")?;
        }
        result
    }

    async fn run_turn_active(
        &mut self,
        task: String,
        on_text: &mut impl FnMut(&str),
    ) -> Result<String> {
        cancellable(Arc::clone(&self.cancel_requested), self.maybe_compact()).await??;
        let definitions = self.tools.definitions();
        self.push_message(Message::text("user", task))?;

        let mut step = 0;
        loop {
            if self.cancel_requested.load(Ordering::Acquire) {
                return Err(TurnCancelled.into());
            }
            step += 1;
            let is_wrap_up = self.max_steps.is_some_and(|max| step > max);

            if is_wrap_up {
                let max = self.max_steps.unwrap();
                if self.event_sink.is_none() {
                    eprintln!("[agent 收尾总结] 达到最大工具循环次数 {max}，正在生成阶段性总结…");
                }
                self.emit(AgentEvent::Step {
                    current: max,
                    total: self.max_steps,
                });
            } else {
                if self.event_sink.is_none() {
                    if let Some(max) = self.max_steps {
                        eprintln!("[agent {step}/{max}] 正在思考…");
                    } else {
                        eprintln!("[agent 步骤 {step}] 正在思考…");
                    }
                }
                self.emit(AgentEvent::Step {
                    current: step,
                    total: self.max_steps,
                });
            }
            let provider = self
                .provider()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("尚未登录 provider，请先使用 /login 登录"))?;

            let summary_messages;
            let (request_definitions, request_messages): (&[Value], &[Message]) = if is_wrap_up {
                let max = self.max_steps.unwrap();
                let mut msgs = self.messages.clone();
                let notice = format!(
                    "\n\n[系统通知：已达到单次回合最大工具调用次数限制（{max} 次），工具已被禁用。请根据当前已执行的操作和已有信息，向用户清晰总结已完成的工作、当前进展及下一步建议。]"
                );
                if let Some(last) = msgs.last_mut() {
                    if last.role == "tool" {
                        if let Some(content) = &mut last.content {
                            content.push_str(&notice);
                        } else {
                            last.content = Some(notice);
                        }
                    } else {
                        msgs.push(Message::text("user", notice.trim().to_owned()));
                    }
                }
                summary_messages = msgs;
                (&[][..], summary_messages.as_slice())
            } else {
                (definitions.as_slice(), self.messages.as_slice())
            };

            let (mut assistant, usage) = cancellable(
                Arc::clone(&self.cancel_requested),
                provider.chat_stream(request_messages, request_definitions, on_text),
            )
            .await??;
            if let Some(usage_info) = usage {
                self.emit(AgentEvent::Usage(usage_info));
            }

            if is_wrap_up {
                let max = self.max_steps.unwrap();
                let notice = format!(
                    "\n\n> ⚠️ *已达到单次回合最大步数限制（{max} 步），以上为阶段性执行总结。您可以输入新指令继续推进。*"
                );
                on_text(&notice);

                let raw_content = assistant.content.take().unwrap_or_default();
                let final_text = if raw_content.trim().is_empty() {
                    format!("已达到单次回合最大步数限制（{max} 步），未能生成进一步总结。{notice}")
                } else {
                    format!("{}{notice}", raw_content.trim())
                };

                assistant.content = Some(final_text.clone());
                assistant.tool_calls = None;
                self.push_message_with_usage(assistant, usage)?;
                self.emit(AgentEvent::Complete);
                return Ok(final_text);
            }

            let calls = assistant.tool_calls.clone().unwrap_or_default();
            let final_text = assistant.content.clone().unwrap_or_default();
            self.push_message_with_usage(assistant, usage)?;

            if calls.is_empty() {
                self.emit(AgentEvent::Complete);
                return if final_text.trim().is_empty() {
                    Err(anyhow::anyhow!("模型既没有返回文本，也没有调用工具"))
                } else {
                    Ok(final_text)
                };
            }

            for call in calls {
                if self.event_sink.is_none() {
                    eprintln!("  → {}", call.function.name);
                }
                self.emit(AgentEvent::ToolStart {
                    name: call.function.name.clone(),
                    args: call.function.arguments.clone(),
                });
                let output = cancellable(
                    Arc::clone(&self.cancel_requested),
                    self.tools
                        .execute(&call.function.name, &call.function.arguments),
                )
                .await?;
                self.emit(AgentEvent::ToolResult {
                    output: output.clone(),
                });
                self.push_message(Message::tool(call.id, output))?;
            }
        }
    }

    fn push_message(&mut self, message: Message) -> Result<()> {
        self.push_message_with_usage(message, None)
    }

    fn push_message_with_usage(
        &mut self,
        message: Message,
        usage: Option<TokenUsage>,
    ) -> Result<()> {
        self.session.append_message_with_usage(&message, usage)?;
        self.messages.push(message);
        Ok(())
    }

    fn emit(&mut self, event: AgentEvent) {
        if let Some(sink) = self.event_sink.as_mut() {
            sink(event);
        }
    }

    async fn maybe_compact(&mut self) -> Result<()> {
        let tokens_before = estimate_tokens(&self.messages);
        if tokens_before <= self.compact_threshold || self.messages.len() < 3 {
            return Ok(());
        }

        let mut recent_tokens = 0;
        let mut split = self.messages.len();
        for index in (1..self.messages.len()).rev() {
            let tokens = estimate_tokens(std::slice::from_ref(&self.messages[index]));
            if recent_tokens + tokens > self.keep_recent_tokens {
                split = index + 1;
                break;
            }
            recent_tokens += tokens;
            split = index;
        }
        while split < self.messages.len() && self.messages[split].role != "user" {
            split += 1;
        }
        if split <= 1 || split >= self.messages.len() {
            return Ok(());
        }

        if self.event_sink.is_none() {
            eprintln!("[context] 正在压缩约 {tokens_before} tokens 的会话…");
        }
        self.emit(AgentEvent::Compacting);
        let history = serde_json::to_string(&self.messages[1..split])?;
        let request = vec![
            Message::text(
                "system",
                "Summarize the coding conversation for continuation. Preserve decisions, changed files, commands, errors, pending work, and important user preferences. Be concise and factual.",
            ),
            Message::text("user", history),
        ];

        let summary = match self
            .provider()
            .ok_or_else(|| anyhow::anyhow!("尚未登录 provider，无法压缩上下文"))?
            .summarize(&request)
            .await
        {
            Ok(summary) => summary,
            Err(error) => {
                if self.event_sink.is_none() {
                    eprintln!("[context] 会话压缩跳过（摘要请求失败: {error:#}）");
                }
                return Ok(());
            }
        };

        let mut compacted = vec![self.messages[0].clone()];
        compacted.push(Message::text(
            "system",
            format!("Previous conversation summary:\n{summary}"),
        ));
        compacted.extend_from_slice(&self.messages[split..]);
        self.messages = compacted;
        self.session
            .append_compaction(tokens_before, summary, &self.messages)?;
        Ok(())
    }

    fn provider(&self) -> Option<&OpenAiProvider> {
        self.active_provider
            .and_then(|index| self.providers.get(index))
    }
}

async fn cancellable<T>(
    cancel_requested: Arc<AtomicBool>,
    future: impl Future<Output = T>,
) -> Result<T> {
    tokio::select! {
        output = future => Ok(output),
        _ = wait_for_cancellation(cancel_requested) => Err(TurnCancelled.into()),
    }
}

async fn wait_for_cancellation(cancel_requested: Arc<AtomicBool>) {
    while !cancel_requested.load(Ordering::Acquire) {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

fn estimate_tokens(messages: &[Message]) -> usize {
    let mut total = 0;
    for message in messages {
        total += 4;
        if let Some(content) = &message.content {
            total += estimate_text_tokens(content);
        }
        if let Some(tool_calls) = &message.tool_calls {
            for call in tool_calls {
                total += 10;
                total += estimate_text_tokens(&call.function.name);
                total += estimate_text_tokens(&call.function.arguments);
            }
        }
    }
    total
}

fn estimate_text_tokens(text: &str) -> usize {
    let mut ascii_count: usize = 0;
    let mut non_ascii_count: usize = 0;
    for ch in text.chars() {
        if ch.is_ascii() {
            ascii_count += 1;
        } else {
            non_ascii_count += 1;
        }
    }
    ascii_count.div_ceil(4) + (non_ascii_count * 5).div_ceil(4)
}

fn env_usize(name: &str, default: usize) -> Result<usize> {
    let value = match env::var(name) {
        Ok(value) => value,
        Err(env::VarError::NotPresent) => return Ok(default),
        Err(error) => return Err(error).with_context(|| format!("无法读取环境变量 {name}")),
    };
    let parsed = value
        .parse::<usize>()
        .with_context(|| format!("{name} 必须是正整数"))?;
    if parsed == 0 {
        anyhow::bail!("{name} 必须大于 0");
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_history_keeps_only_system_prompt() {
        let provider = OpenAiProvider::new(
            "test-key".to_owned(),
            "http://localhost".to_owned(),
            "test-model".to_owned(),
        );
        let mut agent = Agent::new_ephemeral(provider, PathBuf::from("."), Some(3));
        agent.messages.push(Message::text("user", "hello"));

        agent.clear_history().unwrap();

        assert_eq!(agent.messages.len(), 1);
        assert_eq!(agent.messages[0].role, "system");
    }

    #[test]
    fn token_estimate_grows_with_context() {
        let short = vec![Message::text("user", "hi")];
        let long = vec![Message::text("user", "x".repeat(10_000))];
        assert!(estimate_tokens(&long) > estimate_tokens(&short));
    }

    #[tokio::test]
    async fn cancellation_interrupts_a_pending_operation() {
        let cancelled = Arc::new(AtomicBool::new(true));
        let result = cancellable(
            cancelled,
            std::future::pending::<Result<(), std::convert::Infallible>>(),
        )
        .await;

        assert!(matches!(
            result,
            Err(error) if error.downcast_ref::<TurnCancelled>().is_some()
        ));
    }

    #[tokio::test]
    async fn pre_requested_cancellation_keeps_the_previous_context() {
        let provider = OpenAiProvider::new(
            "test-key".to_owned(),
            "http://localhost".to_owned(),
            "test-model".to_owned(),
        );
        let mut agent = Agent::new_ephemeral(provider, PathBuf::from("."), Some(3));
        agent.messages.push(Message::text("user", "previous"));
        agent.cancellation_handle().store(true, Ordering::Release);

        let error = agent
            .run_turn("must not be saved".into(), |_| {})
            .await
            .unwrap_err();

        assert!(error.downcast_ref::<TurnCancelled>().is_some());
        assert_eq!(agent.messages.len(), 2);
        assert_eq!(agent.messages[1].content.as_deref(), Some("previous"));
        assert!(!agent.cancel_requested.load(Ordering::Acquire));
    }

    #[test]
    fn switches_between_configured_providers() {
        let minimax = OpenAiProvider::named(
            "minimax",
            "minimax-key".into(),
            "https://api.minimaxi.com/v1".into(),
            "MiniMax-M3".into(),
        );
        let router = OpenAiProvider::named(
            "9router",
            "router-key".into(),
            "http://localhost:20128/v1".into(),
            "kr/claude-sonnet-4.5".into(),
        );
        let mut agent = Agent::new_ephemeral(minimax, PathBuf::from("."), Some(3));
        agent.providers.push(router);

        let selected = agent.switch_provider(None).unwrap();
        assert_eq!(selected, ("9router".into(), "kr/claude-sonnet-4.5".into()));

        let selected = agent.switch_provider(Some("MINIMAX")).unwrap();
        assert_eq!(selected, ("minimax".into(), "MiniMax-M3".into()));
    }

    #[tokio::test]
    async fn max_steps_triggers_graceful_wrap_up_turn_without_error() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();

        std::thread::spawn(move || {
            // First request: agent calls step 1, provider returns a tool call
            let (mut stream1, _) = listener.accept().unwrap();
            let mut request = [0_u8; 8 * 1024];
            let _ = stream1.read(&mut request);
            let sse1 = concat!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_test\",\"type\":\"function\",\"function\":{\"name\":\"bash\",\"arguments\":\"{\\\"command\\\":\\\"echo ok\\\"}\"}}]}}]}\n\n",
                "data: [DONE]\n\n"
            );
            write!(stream1, "{sse1}").unwrap();
            stream1.flush().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(50));
            drop(stream1);

            // Second request: step 2 > max (max_steps is 1), wrap-up step, provider returns text summary
            let (mut stream2, _) = listener.accept().unwrap();
            let _ = stream2.read(&mut request);
            let sse2 = concat!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"已执行了命令并完成阶段性汇报。\"}}]}\n\n",
                "data: [DONE]\n\n"
            );
            write!(stream2, "{sse2}").unwrap();
            stream2.flush().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(50));
            drop(stream2);
        });

        let provider = OpenAiProvider::new(
            "test-key".to_owned(),
            format!("http://{address}"),
            "test-model".to_owned(),
        );
        let mut agent = Agent::new_ephemeral(provider, PathBuf::from("."), Some(1));
        let mut streamed = String::new();
        let result = agent
            .run_turn("test task".into(), |text| streamed.push_str(text))
            .await;

        let output = result.expect("达到最大步数时不应抛错，应成功返回总结");
        assert!(output.contains("已执行了命令并完成阶段性汇报。"));
        assert!(output.contains("已达到单次回合最大步数限制（1 步）"));
        assert!(streamed.contains("已达到单次回合最大步数限制（1 步）"));

        // 验证消息历史保持完整的闭环: [system, user, assistant(tool_calls), tool(result), assistant(summary)]
        assert_eq!(agent.messages.len(), 5);
        assert_eq!(agent.messages[0].role, "system");
        assert_eq!(agent.messages[1].role, "user");
        assert_eq!(agent.messages[2].role, "assistant");
        assert!(agent.messages[2].tool_calls.is_some());
        assert_eq!(agent.messages[3].role, "tool");
        assert_eq!(agent.messages[4].role, "assistant");
        assert!(agent.messages[4].tool_calls.is_none());
    }
}
