use std::{
    collections::HashMap,
    env,
    future::Future,
    path::{Component, Path, PathBuf},
    pin::Pin,
    time::Duration,
};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::{process::Command, time::timeout};

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
    pub fn coding_tools(root: PathBuf) -> Self {
        let mut registry = Self {
            tools: HashMap::new(),
        };
        registry.register(ReadTool::new(root.clone()));
        registry.register(WriteTool::new(root.clone()));
        registry.register(EditTool::new(root.clone()));
        registry.register(BashTool::new(root));
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
                .min(MAX_BASH_TIMEOUT as usize) as u64;
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

            let child = command.spawn().context("无法执行 shell")?;
            let child_id = child.id();

            let output = match timeout(Duration::from_secs(timeout_seconds), child.wait_with_output()).await {
                Ok(res) => res.context("读取命令输出失败")?,
                Err(_) => {
                    #[cfg(unix)]
                    if let Some(pid) = child_id {
                        let _ = std::process::Command::new("kill")
                            .args(["-KILL", &format!("-{pid}")])
                            .output();
                    }
                    anyhow::bail!("命令超过 {timeout_seconds} 秒，已终止");
                }
            };
            let combined = format!(
                "{}{}{}",
                String::from_utf8_lossy(&output.stdout),
                if output.stdout.is_empty() || output.stderr.is_empty() {
                    ""
                } else {
                    "\n"
                },
                String::from_utf8_lossy(&output.stderr)
            );
            let rendered = if combined.trim().is_empty() {
                "(no output)".to_owned()
            } else {
                combined
            };
            if output.status.success() {
                Ok(rendered)
            } else {
                anyhow::bail!(
                    "{}\nCommand exited with code {}",
                    rendered,
                    output.status.code().unwrap_or(-1)
                )
            }
        })
    }
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
        let registry = ToolRegistry::coding_tools(PathBuf::from("."));
        let names = registry
            .definitions()
            .into_iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, ["bash", "edit", "read", "write"]);
    }

    #[tokio::test]
    async fn edit_requires_exactly_one_match() {
        let root = temp_workspace();
        fs::write(root.join("sample.txt"), "before\nbefore\n").unwrap();
        let registry = ToolRegistry::coding_tools(root.clone());
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
        let registry = ToolRegistry::coding_tools(root.clone());
        let result = registry
            .execute("bash", r#"{"command":"printf rico"}"#)
            .await;
        assert_eq!(result, "rico");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn truncates_utf8_tail_safely() {
        let output = "你".repeat(MAX_OUTPUT_BYTES);
        let truncated = truncate_tail(&output);
        assert!(truncated.starts_with("[Output truncated"));
        assert!(truncated.ends_with('你'));
    }
}
