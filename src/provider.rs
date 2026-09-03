use std::{collections::BTreeMap, time::Duration};

use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn text(role: &str, content: impl Into<String>) -> Self {
        Self {
            role: role.to_owned(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn tool(call_id: String, content: String) -> Self {
        Self {
            role: "tool".to_owned(),
            content: Some(content),
            tool_calls: None,
            tool_call_id: Some(call_id),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
    pub cached_tokens: usize,
}

impl TokenUsage {
    #[allow(dead_code)]
    pub fn cache_hit_rate(&self) -> f64 {
        if self.prompt_tokens == 0 {
            0.0
        } else {
            (self.cached_tokens as f64 / self.prompt_tokens as f64) * 100.0
        }
    }
}

#[derive(Debug, Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [Value]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
}

pub struct OpenAiProvider {
    client: Client,
    api_key: String,
    base_url: String,
    model: String,
}

impl OpenAiProvider {
    pub fn new(api_key: String, base_url: String, model: String) -> Self {
        Self {
            client: Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .build()
                .expect("reqwest client configuration must be valid"),
            api_key,
            base_url: base_url.trim_end_matches('/').to_owned(),
            model,
        }
    }

    pub fn model_name(&self) -> &str {
        &self.model
    }

    pub async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[Value],
        on_text: &mut impl FnMut(&str),
    ) -> Result<(Message, Option<TokenUsage>)> {
        'attempts: for attempt in 0..2 {
            let response = self
                .client
                .post(format!("{}/chat/completions", self.base_url))
                .bearer_auth(&self.api_key)
                .json(&ChatRequest {
                    model: &self.model,
                    messages,
                    tools: (!tools.is_empty()).then_some(tools),
                    tool_choice: (!tools.is_empty()).then_some("auto"),
                    stream: true,
                    stream_options: Some(StreamOptions {
                        include_usage: true,
                    }),
                })
                .send()
                .await;
            let mut response = match response {
                Ok(response) => response,
                Err(error) if attempt == 0 && is_retryable_transport_error(&error) => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
                Err(error) => return Err(error).context("无法连接 OpenAI 兼容 provider"),
            };

            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.context("读取 provider 响应失败")?;
                anyhow::bail!("provider 返回 {status}: {body}");
            }

            let mut buffer = Vec::new();
            let mut content = String::new();
            let mut calls: BTreeMap<usize, ToolCall> = BTreeMap::new();
            let mut usage: Option<TokenUsage> = None;
            let mut stream_complete = false;
            loop {
                let chunk = match response.chunk().await {
                    Ok(Some(chunk)) => chunk,
                    Ok(None) => break,
                    Err(error) if stream_complete && is_tls_close_without_notify(&error) => {
                        break;
                    }
                    Err(error)
                        if attempt == 0
                            && content.is_empty()
                            && calls.is_empty()
                            && is_retryable_transport_error(&error) =>
                    {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        continue 'attempts;
                    }
                    Err(error) => return Err(error).context("读取流式响应失败"),
                };
                buffer.extend_from_slice(&chunk);
                while let Some(end) = find_event_end(&buffer) {
                    let event = buffer.drain(..end).collect::<Vec<_>>();
                    let delimiter = if buffer.starts_with(b"\r\n\r\n") {
                        4
                    } else {
                        2
                    };
                    buffer.drain(..delimiter);
                    stream_complete |= event_completes_stream(&event);
                    process_event(&event, &mut content, &mut calls, &mut usage, on_text)?;
                }
            }
            if !buffer.is_empty() {
                stream_complete |= event_completes_stream(&buffer);
                process_event(&buffer, &mut content, &mut calls, &mut usage, on_text)?;
            }
            if !stream_complete {
                if attempt == 0 && content.is_empty() && calls.is_empty() {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue 'attempts;
                }
                anyhow::bail!("流式响应在完成标记之前结束");
            }

            let tool_calls = (!calls.is_empty()).then(|| calls.into_values().collect());
            return Ok((
                Message {
                    role: "assistant".to_owned(),
                    content: (!content.is_empty()).then_some(content),
                    tool_calls,
                    tool_call_id: None,
                },
                usage,
            ));
        }
        unreachable!("流式请求重试循环至少会返回一次")
    }

    pub async fn summarize(&self, messages: &[Message]) -> Result<String> {
        let mut ignore = |_text: &str| {};
        self.chat_stream(messages, &[], &mut ignore)
            .await?
            .0
            .content
            .context("压缩模型没有返回摘要")
    }
}

fn find_event_end(buffer: &[u8]) -> Option<usize> {
    let lf = buffer.windows(2).position(|window| window == b"\n\n");
    let crlf = buffer.windows(4).position(|window| window == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(index), None) | (None, Some(index)) => Some(index),
        (None, None) => None,
    }
}

fn event_completes_stream(event: &[u8]) -> bool {
    let event = String::from_utf8_lossy(event);
    event.lines().any(|line| {
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            return false;
        };
        if data == "[DONE]" {
            return true;
        }
        serde_json::from_str::<Value>(data)
            .ok()
            .and_then(|value| value.pointer("/choices/0/finish_reason").cloned())
            .is_some_and(|reason| !reason.is_null())
    })
}

fn is_tls_close_without_notify(error: &reqwest::Error) -> bool {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(cause) = current {
        if cause
            .to_string()
            .contains("peer closed connection without sending TLS close_notify")
        {
            return true;
        }
        current = cause.source();
    }
    false
}

fn is_retryable_transport_error(error: &reqwest::Error) -> bool {
    error.is_connect() || error.is_timeout() || is_tls_close_without_notify(error)
}

fn parse_usage(value: &Value) -> Option<TokenUsage> {
    let prompt_tokens = value
        .get("prompt_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let completion_tokens = value
        .get("completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let total_tokens = value
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;

    let cached_tokens = value
        .pointer("/prompt_tokens_details/cached_tokens")
        .or_else(|| value.get("prompt_cache_hit_tokens"))
        .or_else(|| value.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;

    if prompt_tokens > 0 || completion_tokens > 0 || total_tokens > 0 || cached_tokens > 0 {
        Some(TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens: if total_tokens > 0 {
                total_tokens
            } else {
                prompt_tokens + completion_tokens
            },
            cached_tokens,
        })
    } else {
        None
    }
}

fn process_event(
    event: &[u8],
    content: &mut String,
    calls: &mut BTreeMap<usize, ToolCall>,
    usage: &mut Option<TokenUsage>,
    on_text: &mut impl FnMut(&str),
) -> Result<()> {
    let event = String::from_utf8_lossy(event);
    for line in event.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let value: Value = serde_json::from_str(data).context("无法解析 SSE 数据")?;

        if let Some(usage_val) = value.get("usage") {
            if let Some(parsed) = parse_usage(usage_val) {
                *usage = Some(parsed);
            }
        }

        let Some(delta) = value.pointer("/choices/0/delta") else {
            continue;
        };
        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            content.push_str(text);
            on_text(text);
        }
        if let Some(parts) = delta.get("tool_calls").and_then(Value::as_array) {
            for part in parts {
                let index = part.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let call = calls.entry(index).or_insert_with(|| ToolCall {
                    id: String::new(),
                    kind: "function".to_owned(),
                    function: FunctionCall {
                        name: String::new(),
                        arguments: String::new(),
                    },
                });
                if let Some(id) = part.get("id").and_then(Value::as_str) {
                    if call.id.is_empty() {
                        call.id = id.to_owned();
                    } else if !call.id.contains(id) && !id.contains(&call.id) {
                        call.id.push_str(id);
                    }
                }
                if let Some(kind) = part.get("type").and_then(Value::as_str) {
                    call.kind = kind.to_owned();
                }
                if let Some(function) = part.get("function") {
                    if let Some(name) = function.get("name").and_then(Value::as_str) {
                        if call.function.name.is_empty() {
                            call.function.name = name.to_owned();
                        } else if call.function.name != name
                            && !name.is_empty()
                            && !call.function.name.ends_with(name)
                        {
                            call.function.name.push_str(name);
                        }
                    }
                    if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                        call.function.arguments.push_str(arguments);
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
        time::Duration as StdDuration,
    };

    fn serve_sse_once(body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(StdDuration::from_secs(2)))
                .unwrap();
            let mut request = [0_u8; 8 * 1024];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        format!("http://{address}")
    }

    #[test]
    fn rebuilds_streamed_text_and_tool_calls() {
        let events = [
            r#"data: {"choices":[{"delta":{"content":"你"}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"ba","arguments":"{\"com"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"sh","arguments":"mand\":\"pwd\"}"}}]}}]}"#,
        ];
        let mut content = String::new();
        let mut streamed = String::new();
        let mut calls = BTreeMap::new();
        let mut usage = None;
        for event in events {
            process_event(
                event.as_bytes(),
                &mut content,
                &mut calls,
                &mut usage,
                &mut |text| streamed.push_str(text),
            )
            .unwrap();
        }
        let call = calls.get(&0).unwrap();
        assert_eq!(content, "你");
        assert_eq!(streamed, "你");
        assert_eq!(call.function.name, "bash");
        assert_eq!(call.function.arguments, r#"{"command":"pwd"}"#);
        assert!(usage.is_none());
    }

    #[test]
    fn parses_openai_prompt_cache_usage() {
        let event = r#"data: {"choices":[],"usage":{"prompt_tokens":1200,"completion_tokens":80,"total_tokens":1280,"prompt_tokens_details":{"cached_tokens":1024}}}"#;
        let mut content = String::new();
        let mut calls = BTreeMap::new();
        let mut usage = None;
        process_event(
            event.as_bytes(),
            &mut content,
            &mut calls,
            &mut usage,
            &mut |_| {},
        )
        .unwrap();

        let usage = usage.expect("应成功解析 usage");
        assert_eq!(usage.prompt_tokens, 1200);
        assert_eq!(usage.completion_tokens, 80);
        assert_eq!(usage.total_tokens, 1280);
        assert_eq!(usage.cached_tokens, 1024);
        assert!((usage.cache_hit_rate() - 85.33).abs() < 0.1);
    }

    #[test]
    fn parses_deepseek_prompt_cache_hit_tokens() {
        let event = r#"data: {"choices":[],"usage":{"prompt_tokens":500,"completion_tokens":50,"total_tokens":550,"prompt_cache_hit_tokens":250}}"#;
        let mut content = String::new();
        let mut calls = BTreeMap::new();
        let mut usage = None;
        process_event(
            event.as_bytes(),
            &mut content,
            &mut calls,
            &mut usage,
            &mut |_| {},
        )
        .unwrap();

        let usage = usage.expect("应成功解析 deepseek usage");
        assert_eq!(usage.cached_tokens, 250);
        assert_eq!(usage.cache_hit_rate(), 50.0);
    }

    #[test]
    fn deduplicates_repeated_tool_call_id_and_name() {
        let events = [
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_123","type":"function","function":{"name":"bash","arguments":"{\"com"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_123","function":{"name":"bash","arguments":"mand\":\"pwd\"}"}}]}}]}"#,
        ];
        let mut content = String::new();
        let mut streamed = String::new();
        let mut calls = BTreeMap::new();
        let mut usage = None;
        for event in events {
            process_event(
                event.as_bytes(),
                &mut content,
                &mut calls,
                &mut usage,
                &mut |text| streamed.push_str(text),
            )
            .unwrap();
        }
        let call = calls.get(&0).unwrap();
        assert_eq!(call.id, "call_123");
        assert_eq!(call.function.name, "bash");
        assert_eq!(call.function.arguments, r#"{"command":"pwd"}"#);
    }

    #[test]
    fn finds_earliest_sse_delimiter() {
        assert_eq!(find_event_end(b"first\r\n\r\nsecond\n\n"), Some(5));
    }

    #[test]
    fn recognizes_protocol_level_stream_completion() {
        assert!(event_completes_stream(b"data: [DONE]"));
        assert!(event_completes_stream(
            br#"data: {"choices":[{"finish_reason":"stop","delta":{}}]}"#
        ));
        assert!(event_completes_stream(
            br#"data: {"choices":[{"finish_reason":"tool_calls","delta":{}}]}"#
        ));
        assert!(!event_completes_stream(
            br#"data: {"choices":[{"finish_reason":null,"delta":{"content":"hi"}}]}"#
        ));
        assert!(!event_completes_stream(
            br#"data: {"choices":[{"delta":{"content":"truncated"}}]}"#
        ));
    }

    #[tokio::test]
    async fn rejects_stream_that_ends_without_completion_marker() {
        let base_url =
            serve_sse_once("data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n");
        let provider = OpenAiProvider::new("test-key".into(), base_url, "test-model".into());
        let mut streamed = String::new();
        let error = provider
            .chat_stream(&[], &[], &mut |text| streamed.push_str(text))
            .await
            .unwrap_err();
        assert_eq!(streamed, "partial");
        assert!(error.to_string().contains("完成标记"));
    }
}
