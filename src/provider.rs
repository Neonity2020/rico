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

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [Value]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
    stream: bool,
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
    ) -> Result<Message> {
        let mut response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&ChatRequest {
                model: &self.model,
                messages,
                tools: (!tools.is_empty()).then_some(tools),
                tool_choice: (!tools.is_empty()).then_some("auto"),
                stream: true,
            })
            .send()
            .await
            .context("无法连接 OpenAI 兼容 provider")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.context("读取 provider 响应失败")?;
            anyhow::bail!("provider 返回 {status}: {body}");
        }

        let mut buffer = Vec::new();
        let mut content = String::new();
        let mut calls: BTreeMap<usize, ToolCall> = BTreeMap::new();
        while let Some(chunk) = response.chunk().await.context("读取流式响应失败")? {
            buffer.extend_from_slice(&chunk);
            while let Some(end) = find_event_end(&buffer) {
                let event = buffer.drain(..end).collect::<Vec<_>>();
                let delimiter = if buffer.starts_with(b"\r\n\r\n") {
                    4
                } else {
                    2
                };
                buffer.drain(..delimiter);
                process_event(&event, &mut content, &mut calls, on_text)?;
            }
        }
        if !buffer.is_empty() {
            process_event(&buffer, &mut content, &mut calls, on_text)?;
        }

        let tool_calls = (!calls.is_empty()).then(|| calls.into_values().collect());
        Ok(Message {
            role: "assistant".to_owned(),
            content: (!content.is_empty()).then_some(content),
            tool_calls,
            tool_call_id: None,
        })
    }

    pub async fn summarize(&self, messages: &[Message]) -> Result<String> {
        let mut ignore = |_text: &str| {};
        self.chat_stream(messages, &[], &mut ignore)
            .await?
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

fn process_event(
    event: &[u8],
    content: &mut String,
    calls: &mut BTreeMap<usize, ToolCall>,
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
        for event in events {
            process_event(event.as_bytes(), &mut content, &mut calls, &mut |text| {
                streamed.push_str(text)
            })
            .unwrap();
        }
        let call = calls.get(&0).unwrap();
        assert_eq!(content, "你");
        assert_eq!(streamed, "你");
        assert_eq!(call.function.name, "bash");
        assert_eq!(call.function.arguments, r#"{"command":"pwd"}"#);
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
        for event in events {
            process_event(event.as_bytes(), &mut content, &mut calls, &mut |text| {
                streamed.push_str(text)
            })
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
}
