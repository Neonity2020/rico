use std::{env, path::PathBuf};

use anyhow::Result;

use crate::{
    provider::{Message, OpenAiProvider, TokenUsage},
    session::{CacheStats, SessionStore},
    tools::ToolRegistry,
};

const SYSTEM_PROMPT: &str = r#"你是一个最小但可靠的 coding agent。你的工作目录就是项目根目录。
先检查相关文件，再做修改；修改后运行合适的检查或测试。优先使用 read/edit/write 操作文件，使用 bash 执行命令、搜索和必要的网络请求。
工具失败时，阅读错误并尝试安全的替代方案。完成后简洁说明改了什么以及验证结果。"#;

/// Hook events emitted by [`Agent`] during a turn. The TUI subscribes to these
/// to drive its interface; the REPL ignores them.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    Delta(String),
    ToolStart { name: String, args: String },
    ToolResult { output: String },
    Step { current: usize, total: usize },
    Compacting,
    Complete,
    Error(String),
    Usage(TokenUsage),
}

pub struct Agent {
    provider: OpenAiProvider,
    tools: ToolRegistry,
    max_steps: usize,
    messages: Vec<Message>,
    session: SessionStore,
    compact_threshold: usize,
    keep_recent_tokens: usize,
    event_sink: Option<Box<dyn FnMut(AgentEvent) + Send>>,
}

impl Agent {
    pub fn new(
        provider: OpenAiProvider,
        workspace: PathBuf,
        max_steps: usize,
        resume: bool,
    ) -> Result<Self> {
        let system = Message::text("system", SYSTEM_PROMPT);
        let (mut session, mut messages) = SessionStore::open(&workspace, resume, system.clone())?;
        if messages.is_empty() {
            messages.push(system.clone());
            session.append_message(&system)?;
        }
        Ok(Self {
            provider,
            tools: ToolRegistry::coding_tools(workspace),
            max_steps,
            messages,
            session,
            compact_threshold: env_usize("RICO_COMPACT_TOKENS", 200_000),
            keep_recent_tokens: env_usize("RICO_KEEP_RECENT_TOKENS", 20_000),
            event_sink: None,
        })
    }

    #[cfg(test)]
    fn new_ephemeral(provider: OpenAiProvider, workspace: PathBuf, max_steps: usize) -> Self {
        Self {
            provider,
            tools: ToolRegistry::coding_tools(workspace),
            max_steps,
            messages: vec![Message::text("system", SYSTEM_PROMPT)],
            session: SessionStore::ephemeral(),
            compact_threshold: 200_000,
            keep_recent_tokens: 20_000,
            event_sink: None,
        }
    }

    pub fn set_event_sink(&mut self, sink: Option<Box<dyn FnMut(AgentEvent) + Send>>) {
        self.event_sink = sink;
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
        self.provider.model_name()
    }

    pub async fn run_turn(
        &mut self,
        task: String,
        mut on_text: impl FnMut(&str),
    ) -> Result<String> {
        self.maybe_compact().await?;
        let definitions = self.tools.definitions();
        self.push_message(Message::text("user", task))?;

        for step in 1..=self.max_steps {
            if self.event_sink.is_none() {
                eprintln!("[agent {step}/{}] 正在思考…", self.max_steps);
            }
            self.emit(AgentEvent::Step {
                current: step,
                total: self.max_steps,
            });
            let (assistant, usage) = self
                .provider
                .chat_stream(&self.messages, &definitions, &mut on_text)
                .await?;
            if let Some(usage_info) = usage {
                self.emit(AgentEvent::Usage(usage_info));
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
                let output = self
                    .tools
                    .execute(&call.function.name, &call.function.arguments)
                    .await;
                self.emit(AgentEvent::ToolResult {
                    output: output.clone(),
                });
                self.push_message(Message::tool(call.id, output))?;
            }
        }

        let err = anyhow::anyhow!("达到最大工具循环次数 {}", self.max_steps)
            .context("agent 未能在限制内完成任务");
        self.emit(AgentEvent::Error(format!("{err:#}")));
        Err(err)
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

        let summary = match self.provider.summarize(&request).await {
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

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
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
        let mut agent = Agent::new_ephemeral(provider, PathBuf::from("."), 3);
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
}
