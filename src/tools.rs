use std::{
    collections::{HashMap, VecDeque},
    env,
    future::Future,
    path::{Component, Path, PathBuf},
    pin::Pin,
    time::Duration,
};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    time::timeout,
};

const DEFAULT_READ_LINES: usize = 2_000;
const MAX_OUTPUT_BYTES: usize = 50 * 1024;
const MAX_OUTPUT_LINES: usize = 2_000;
const DEFAULT_BASH_TIMEOUT: u64 = 120;
const MAX_BASH_TIMEOUT: u64 = 600;

type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>>;

trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn parameters(&self) -> Value;
    fn execute<'a>(&'a self, arguments: &'a str) -> ToolFuture<'a>;

    fn definition(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": self.name(),
                "description": self.description(),
                "parameters": self.parameters()
            }
        })
    }
}

pub struct ToolRegistry {
    tools: HashMap<&'static str, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn coding_tools(root: PathBuf, exa_api_key: Option<String>) -> Self {
        let mut registry = Self {
            tools: HashMap::new(),
        };
        registry.register(ReadTool::new(root.clone()));
        registry.register(WriteTool::new(root.clone()));
        registry.register(EditTool::new(root.clone()));
        registry.register(BashTool::new(root));
        registry.register(WebSearchTool::new(exa_api_key));
        registry
    }

    fn register(&mut self, tool: impl Tool + 'static) {
        self.tools.insert(tool.name(), Box::new(tool));
    }

    pub fn definitions(&self) -> Vec<Value> {
        let mut tools = self.tools.values().collect::<Vec<_>>();
        tools.sort_by_key(|tool| tool.name());
        tools.into_iter().map(|tool| tool.definition()).collect()
    }

    pub async fn execute(&self, name: &str, arguments: &str) -> String {
        let Some(tool) = self.tools.get(name) else {
            return format!("error: 未知工具: {name}");
        };
        match tool.execute(arguments).await {
            Ok(output) => truncate_tail(&output),
            Err(error) => format!("error: {error:#}"),
        }
    }
}

struct Workspace {
    root: PathBuf,
}

impl Workspace {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn path(&self, relative: &str) -> Result<PathBuf> {
        let path = Path::new(relative);
        if path.is_absolute()
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::Prefix(_)))
        {
            anyhow::bail!("路径必须位于工作区内且不能包含 ..");
        }
        if is_sensitive_path(path) {
            anyhow::bail!("拒绝访问密钥或凭据文件");
        }

        let mut resolved = self.root.clone();
        for component in path.components() {
            if let Component::Normal(part) = component {
                resolved.push(part);
                if let Ok(metadata) = std::fs::symlink_metadata(&resolved) {
                    if metadata.file_type().is_symlink() {
                        anyhow::bail!("拒绝访问符号链接路径");
                    }
                }
            }
        }
        Ok(resolved)
    }
}

struct ReadTool(Workspace);

impl ReadTool {
    fn new(root: PathBuf) -> Self {
        Self(Workspace::new(root))
    }
}

impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        "read"
    }

    fn description(&self) -> &'static str {
        "Read a UTF-8 text file in the workspace. Supports a 1-based line offset and line limit."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "offset": {"type": "integer", "minimum": 1},
                "limit": {"type": "integer", "minimum": 1}
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn execute<'a>(&'a self, arguments: &'a str) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: Value = serde_json::from_str(arguments)?;
            let path = self.0.path(required_str(&args, "path")?)?;
            let offset = optional_usize(&args, "offset")?.unwrap_or(1);
            let limit = optional_usize(&args, "limit")?.unwrap_or(DEFAULT_READ_LINES);
            let content = tokio::fs::read_to_string(&path)
                .await
                .with_context(|| format!("无法读取 {}", path.display()))?;
            let lines = content
                .lines()
                .skip(offset.saturating_sub(1))
                .take(limit)
                .enumerate()
                .map(|(index, line)| format!("{:>6}\t{}", offset + index, truncate_line(line)))
                .collect::<Vec<_>>()
                .join("\n");
            Ok(if lines.is_empty() {
                "(empty or offset beyond end of file)".to_owned()
            } else {
                lines
            })
        })
    }
}

struct WriteTool(Workspace);

impl WriteTool {
    fn new(root: PathBuf) -> Self {
        Self(Workspace::new(root))
    }
}

impl Tool for WriteTool {
    fn name(&self) -> &'static str {
        "write"
    }

    fn description(&self) -> &'static str {
        "Create or completely overwrite a UTF-8 text file in the workspace."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"}
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })
    }

    fn execute<'a>(&'a self, arguments: &'a str) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: Value = serde_json::from_str(arguments)?;
            let relative = required_str(&args, "path")?;
            let content = required_str(&args, "content")?;
            let path = self.0.path(relative)?;
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&path, content).await?;
            Ok(format!("Wrote {} bytes to {relative}", content.len()))
        })
    }
}

struct EditTool(Workspace);

impl EditTool {
    fn new(root: PathBuf) -> Self {
        Self(Workspace::new(root))
    }
}

impl Tool for EditTool {
    fn name(&self) -> &'static str {
        "edit"
    }

    fn description(&self) -> &'static str {
        "Replace exactly one occurrence of old_text in a workspace text file."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "old_text": {"type": "string"},
                "new_text": {"type": "string"}
            },
            "required": ["path", "old_text", "new_text"],
            "additionalProperties": false
        })
    }

    fn execute<'a>(&'a self, arguments: &'a str) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: Value = serde_json::from_str(arguments)?;
            let relative = required_str(&args, "path")?;
            let old_text = required_str(&args, "old_text")?;
            let new_text = required_str(&args, "new_text")?;
            if old_text.is_empty() {
                anyhow::bail!("old_text 不能为空");
            }
            let path = self.0.path(relative)?;
            let content = tokio::fs::read_to_string(&path).await?;
            let matches = content.match_indices(old_text).count();
            if matches != 1 {
                anyhow::bail!("old_text 必须恰好匹配一次，实际匹配 {matches} 次");
            }
            let updated = content.replacen(old_text, new_text, 1);
            tokio::fs::write(&path, updated).await?;
            Ok(format!("Edited {relative}"))
        })
    }
}

struct BashTool {
    root: PathBuf,
}

impl BashTool {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Tool for BashTool {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn description(&self) -> &'static str {
        "Execute a shell command in the workspace. Network and normal shell features are available. Output is truncated."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string"},
                "timeout": {"type": "integer", "minimum": 1, "maximum": MAX_BASH_TIMEOUT}
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    fn execute<'a>(&'a self, arguments: &'a str) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: Value = serde_json::from_str(arguments)?;
            let command_text = required_str(&args, "command")?;
            let timeout_seconds = optional_usize(&args, "timeout")?
                .unwrap_or(DEFAULT_BASH_TIMEOUT as usize)
                .clamp(1, MAX_BASH_TIMEOUT as usize) as u64;
            let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
            let mut command = Command::new(shell);
            command
                .args(["-c", command_text])
                .current_dir(&self.root)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true);

            #[cfg(unix)]
            {
                command.process_group(0);
            }

            let mut child = command.spawn().context("无法执行 shell")?;
            let child_id = child.id();
            let stdout = child.stdout.take().context("无法捕获命令标准输出")?;
            let stderr = child.stderr.take().context("无法捕获命令错误输出")?;
            let stdout_task = tokio::spawn(read_bounded_tail(stdout));
            let stderr_task = tokio::spawn(read_bounded_tail(stderr));

            let status = match timeout(Duration::from_secs(timeout_seconds), child.wait()).await {
                Ok(res) => res.context("等待命令结束失败")?,
                Err(_) => {
                    #[cfg(unix)]
                    if let Some(pid) = child_id {
                        let _ = std::process::Command::new("kill")
                            .args(["-KILL", &format!("-{pid}")])
                            .output();
                    }
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    let _ = stdout_task.await;
                    let _ = stderr_task.await;
                    anyhow::bail!("命令超过 {timeout_seconds} 秒，已终止");
                }
            };
            let stdout = stdout_task.await.context("标准输出读取任务失败")??;
            let stderr = stderr_task.await.context("错误输出读取任务失败")??;
            let combined = format!(
                "{}{}{}",
                String::from_utf8_lossy(&stdout),
                if stdout.is_empty() || stderr.is_empty() {
                    ""
                } else {
                    "\n"
                },
                String::from_utf8_lossy(&stderr)
            );
            let rendered = if combined.trim().is_empty() {
                "(no output)".to_owned()
            } else {
                combined
            };
            if status.success() {
                Ok(rendered)
            } else {
                anyhow::bail!(
                    "{}\nCommand exited with code {}",
                    rendered,
                    status.code().unwrap_or(-1)
                )
            }
        })
    }
}

pub struct WebSearchTool {
    api_key: Option<String>,
    auth_path: Option<PathBuf>,
    client: reqwest::Client,
}

impl WebSearchTool {
    pub fn new(api_key: Option<String>) -> Self {
        Self {
            api_key,
            auth_path: crate::auth::auth_path().ok(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
        }
    }

    fn resolve_api_key(&self) -> Option<String> {
        if let Some(ref key) = self.api_key {
            if !key.trim().is_empty() && !key.starts_with("your-") {
                return Some(key.clone());
            }
        }
        if let Ok(key) = env::var("EXA_API_KEY") {
            if !key.trim().is_empty() && !key.starts_with("your-") {
                return Some(key);
            }
        }
        if let Some(path) = self.auth_path.clone() {
            if let Ok(store) = crate::auth::AuthStore::load(path) {
                if let Some(key) = store.api_key("exa") {
                    if !key.trim().is_empty() && !key.starts_with("your-") {
                        return Some(key);
                    }
                }
            }
        }
        None
    }
}

#[derive(Deserialize)]
struct ExaSearchResponse {
    #[serde(default)]
    results: Vec<ExaSearchResult>,
}

#[derive(Deserialize)]
struct ExaSearchResult {
    title: Option<String>,
    url: String,
    #[serde(default)]
    highlights: Vec<String>,
    text: Option<String>,
}

impl Tool for WebSearchTool {
    fn name(&self) -> &'static str {
        "web_search"
    }

    fn description(&self) -> &'static str {
        "Search the web using Exa AI search engine. Returns relevant webpages with titles, URLs, and concise highlights or text snippets."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                },
                "num_results": {
                    "type": "integer",
                    "description": "Number of search results to return (1-10, default: 5)",
                    "minimum": 1,
                    "maximum": 10
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn execute<'a>(&'a self, arguments: &'a str) -> ToolFuture<'a> {
        Box::pin(async move {
            let Some(api_key) = self.resolve_api_key() else {
                anyhow::bail!(
                    "未配置 EXA_API_KEY。请在 ~/.config/rico/config.env 或 .env 中设置 EXA_API_KEY=your_key，或存入 auth.json (exa 凭证)。"
                );
            };

            let args: Value = serde_json::from_str(arguments).context("无效的 JSON 参数")?;
            let query = required_str(&args, "query")?;
            let num_results = args
                .get("num_results")
                .and_then(Value::as_u64)
                .map(|n| n.clamp(1, 10))
                .unwrap_or(5);

            let body = json!({
                "query": query,
                "numResults": num_results,
                "contents": {
                    "highlights": true,
                    "text": true
                }
            });

            let response = self
                .client
                .post("https://api.exa.ai/search")
                .header("x-api-key", &api_key)
                .header("Authorization", format!("Bearer {api_key}"))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .context("发送 Exa 搜索请求失败")?;

            let status = response.status();
            if !status.is_success() {
                let error_text = response.text().await.unwrap_or_default();
                anyhow::bail!("Exa API 请求失败 ({status}): {error_text}");
            }

            let search_response: ExaSearchResponse = response
                .json()
                .await
                .context("解析 Exa 搜索响应 JSON 失败")?;

            if search_response.results.is_empty() {
                return Ok("未找到相关搜索结果。".to_string());
            }

            let mut output = String::new();
            for (index, result) in search_response.results.iter().enumerate() {
                let title = result.title.as_deref().unwrap_or("Untitled").trim();
                output.push_str(&format!("{}. [{}]({})\n", index + 1, title, result.url));

                if !result.highlights.is_empty() {
                    for highlight in &result.highlights {
                        let h = highlight.trim();
                        if !h.is_empty() {
                            output.push_str(&format!("   - {h}\n"));
                        }
                    }
                } else if let Some(text) = &result.text {
                    let snippet = text.trim();
                    if !snippet.is_empty() {
                        let single_line = snippet.replace(['\r', '\n'], " ");
                        let truncated: String = single_line.chars().take(300).collect();
                        output.push_str(&format!("   {truncated}…\n"));
                    }
                }
                output.push('\n');
            }

            Ok(output.trim_end().to_string())
        })
    }
}

async fn read_bounded_tail(mut reader: impl AsyncRead + Unpin) -> std::io::Result<Vec<u8>> {
    let mut tail = VecDeque::with_capacity(MAX_OUTPUT_BYTES);
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        tail.extend(&chunk[..read]);
        let excess = tail.len().saturating_sub(MAX_OUTPUT_BYTES);
        tail.drain(..excess);
    }
    Ok(tail.into())
}

fn required_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("缺少字符串参数 {key}"))
}

fn optional_usize(value: &Value, key: &str) -> Result<Option<usize>> {
    value
        .get(key)
        .map(|value| {
            value
                .as_u64()
                .context("参数必须是正整数")
                .and_then(|value| usize::try_from(value).context("整数过大"))
        })
        .transpose()
}

fn truncate_line(line: &str) -> &str {
    let mut boundary = line.len().min(2_000);
    while !line.is_char_boundary(boundary) {
        boundary -= 1;
    }
    &line[..boundary]
}

fn truncate_tail(output: &str) -> String {
    let lines = output.lines().collect::<Vec<_>>();
    let start_line = lines.len().saturating_sub(MAX_OUTPUT_LINES);
    let by_lines = lines[start_line..].join("\n");
    if by_lines.len() <= MAX_OUTPUT_BYTES && start_line == 0 {
        return by_lines;
    }

    let mut start_byte = by_lines.len().saturating_sub(MAX_OUTPUT_BYTES);
    while !by_lines.is_char_boundary(start_byte) {
        start_byte += 1;
    }
    let content = &by_lines[start_byte..];
    format!(
        "[Output truncated: showing the tail]\n{}",
        content.trim_start_matches('\n')
    )
}

fn is_sensitive_path(path: &Path) -> bool {
    path.components().any(|component| {
        let Component::Normal(name) = component else {
            return false;
        };
        let name = name.to_string_lossy().to_ascii_lowercase();
        name == ".env"
            || name.starts_with(".env.")
            || matches!(
                name.as_str(),
                ".aws"
                    | ".git"
                    | ".netrc"
                    | ".npmrc"
                    | ".pypirc"
                    | ".ssh"
                    | "auth.json"
                    | "credentials"
                    | "id_ed25519"
                    | "id_rsa"
                    | "id_ecdsa"
                    | "id_dsa"
                    | "config.env"
            )
            || name.ends_with(".key")
            || name.ends_with(".pem")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
        time::SystemTime,
    };

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_workspace() -> PathBuf {
        let count = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "rico-tools-{}-{unique}-{count}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn registry_has_pi_coding_tools() {
        let registry = ToolRegistry::coding_tools(PathBuf::from("."), None);
        let names = registry
            .definitions()
            .into_iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, ["bash", "edit", "read", "web_search", "write"]);
    }

    #[tokio::test]
    async fn web_search_without_key_reports_helpful_error() {
        let mut tool = WebSearchTool::new(None);
        tool.auth_path = None;
        let result = tool.execute(r#"{"query":"rust async"}"#).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("未配置 EXA_API_KEY"));
    }

    #[tokio::test]
    async fn edit_requires_exactly_one_match() {
        let root = temp_workspace();
        fs::write(root.join("sample.txt"), "before\nbefore\n").unwrap();
        let registry = ToolRegistry::coding_tools(root.clone(), None);
        let result = registry
            .execute(
                "edit",
                r#"{"path":"sample.txt","old_text":"before","new_text":"after"}"#,
            )
            .await;
        assert!(result.contains("匹配 2 次"));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn bash_executes_shell_commands() {
        let root = temp_workspace();
        let registry = ToolRegistry::coding_tools(root.clone(), None);
        let result = registry
            .execute("bash", r#"{"command":"printf rico"}"#)
            .await;
        assert_eq!(result, "rico");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn command_output_is_bounded_while_reading() {
        let reader =
            tokio::io::AsyncReadExt::take(tokio::io::repeat(b'x'), (MAX_OUTPUT_BYTES * 3) as u64);
        let output = read_bounded_tail(reader).await.unwrap();
        assert_eq!(output.len(), MAX_OUTPUT_BYTES);
        assert!(output.iter().all(|byte| *byte == b'x'));
    }

    #[test]
    fn workspace_rejects_escape_and_sensitive_paths() {
        let workspace = Workspace::new(PathBuf::from("/tmp/workspace"));
        assert!(workspace.path("../secret").is_err());
        assert!(workspace.path(".env").is_err());
        assert!(workspace.path("auth.json").is_err());
        assert!(workspace.path("nested/id_rsa").is_err());
    }

    #[test]
    fn truncates_utf8_tail_safely() {
        let output = "你".repeat(MAX_OUTPUT_BYTES);
        let truncated = truncate_tail(&output);
        assert!(truncated.starts_with("[Output truncated"));
        assert!(truncated.ends_with('你'));
    }
}
