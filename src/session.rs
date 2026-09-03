use std::{
    env,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::provider::{Message, TokenUsage};

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct CacheStats {
    pub total_prompt_tokens: usize,
    pub total_completion_tokens: usize,
    pub total_cached_tokens: usize,
    pub latest_prompt_tokens: usize,
    pub latest_cached_tokens: usize,
    pub requests_count: usize,
    pub cache_hit_requests_count: usize,
}

impl CacheStats {
    pub fn overall_hit_rate(&self) -> f64 {
        if self.total_prompt_tokens == 0 {
            0.0
        } else {
            (self.total_cached_tokens as f64 / self.total_prompt_tokens as f64) * 100.0
        }
    }

    pub fn latest_hit_rate(&self) -> f64 {
        if self.latest_prompt_tokens == 0 {
            0.0
        } else {
            (self.latest_cached_tokens as f64 / self.latest_prompt_tokens as f64) * 100.0
        }
    }

    pub fn uncached_prompt_tokens(&self) -> usize {
        self.total_prompt_tokens
            .saturating_sub(self.total_cached_tokens)
    }

    pub fn total_tokens(&self) -> usize {
        self.total_prompt_tokens + self.total_completion_tokens
    }

    pub fn record_usage(&mut self, usage: &TokenUsage) {
        self.requests_count += 1;
        self.total_prompt_tokens += usage.prompt_tokens;
        self.total_completion_tokens += usage.completion_tokens;
        self.total_cached_tokens += usage.cached_tokens;
        self.latest_prompt_tokens = usage.prompt_tokens;
        self.latest_cached_tokens = usage.cached_tokens;
        if usage.cached_tokens > 0 {
            self.cache_hit_requests_count += 1;
        }
    }

    pub fn summary_text(&self) -> String {
        if self.requests_count == 0 {
            return "暂无模型请求的缓存命中记录。".to_string();
        }
        format!(
            "Prompt 缓存命中统计：\n  总请求次数: {} 次（命中 {} 次）\n  输入词元 (Prompt): {}\n  ├─ 命中缓存 (Cached): {} ({:.1}%)\n  └─ 未命中 (Uncached): {}\n  输出词元 (Completion): {}\n  总计消耗 (Total): {}\n  最近一轮命中率: {:.1}% ({} / {})",
            self.requests_count,
            self.cache_hit_requests_count,
            self.total_prompt_tokens,
            self.total_cached_tokens,
            self.overall_hit_rate(),
            self.uncached_prompt_tokens(),
            self.total_completion_tokens,
            self.total_tokens(),
            self.latest_hit_rate(),
            self.latest_cached_tokens,
            self.latest_prompt_tokens
        )
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SessionEntry {
    Header {
        version: u32,
        workspace: String,
        created_at: u64,
    },
    Message {
        timestamp: u64,
        message: Message,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<TokenUsage>,
    },
    Compaction {
        timestamp: u64,
        tokens_before: usize,
        summary: String,
        active_messages: Vec<Message>,
    },
    Reset {
        timestamp: u64,
    },
}

pub struct SessionStore {
    path: Option<PathBuf>,
    cache_stats: CacheStats,
}

impl SessionStore {
    pub fn open(workspace: &Path, resume: bool, system: Message) -> Result<(Self, Vec<Message>)> {
        let directory = session_directory(workspace)?;
        fs::create_dir_all(&directory)?;

        if resume {
            if let Some(path) = latest_session(&directory)? {
                let (messages, cache_stats) = load_messages(&path)?;
                return Ok((
                    Self {
                        path: Some(path),
                        cache_stats,
                    },
                    messages,
                ));
            }
        }

        let path = directory.join(format!("{}-{}.jsonl", now_nanos(), std::process::id()));
        let mut store = Self {
            path: Some(path),
            cache_stats: CacheStats::default(),
        };
        store.append(&SessionEntry::Header {
            version: 1,
            workspace: workspace.display().to_string(),
            created_at: now_millis(),
        })?;
        store.append_message(&system)?;
        Ok((store, vec![system]))
    }

    #[cfg(test)]
    pub fn ephemeral() -> Self {
        Self {
            path: None,
            cache_stats: CacheStats::default(),
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn cache_stats(&self) -> CacheStats {
        self.cache_stats
    }

    pub fn append_message(&mut self, message: &Message) -> Result<()> {
        self.append_message_with_usage(message, None)
    }

    pub fn append_message_with_usage(
        &mut self,
        message: &Message,
        usage: Option<TokenUsage>,
    ) -> Result<()> {
        if let Some(usage_info) = usage {
            self.cache_stats.record_usage(&usage_info);
        }
        self.append(&SessionEntry::Message {
            timestamp: now_millis(),
            message: message.clone(),
            usage,
        })
    }

    pub fn append_compaction(
        &self,
        tokens_before: usize,
        summary: String,
        active_messages: &[Message],
    ) -> Result<()> {
        self.append(&SessionEntry::Compaction {
            timestamp: now_millis(),
            tokens_before,
            summary,
            active_messages: active_messages.to_vec(),
        })
    }

    pub fn append_reset(&mut self) -> Result<()> {
        self.cache_stats = CacheStats::default();
        self.append(&SessionEntry::Reset {
            timestamp: now_millis(),
        })
    }

    fn append(&self, entry: &SessionEntry) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        serde_json::to_writer(&mut file, entry)?;
        writeln!(file)?;
        file.flush()?;
        Ok(())
    }
}

fn session_directory(workspace: &Path) -> Result<PathBuf> {
    if let Some(path) = env::var_os("RICO_SESSION_DIR") {
        return Ok(PathBuf::from(path).join(workspace_key(workspace)));
    }
    let home = env::var_os("HOME").context("无法确定 HOME，请设置 RICO_SESSION_DIR")?;
    Ok(PathBuf::from(home)
        .join(".local/share/rico/sessions")
        .join(workspace_key(workspace)))
}

fn fnv1a_hash(data: &[u8]) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET_BASIS;
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn workspace_key(workspace: &Path) -> String {
    let hash = fnv1a_hash(workspace.to_string_lossy().as_bytes());
    let name = workspace
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace");
    format!("{name}-{hash:016x}")
}

fn latest_session(directory: &Path) -> Result<Option<PathBuf>> {
    let mut entries = fs::read_dir(directory)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .collect::<Vec<_>>();
    entries.sort_by_key(|path| fs::metadata(path).and_then(|meta| meta.modified()).ok());
    Ok(entries.pop())
}

fn load_messages(path: &Path) -> Result<(Vec<Message>, CacheStats)> {
    let file = fs::File::open(path)?;
    let mut messages = Vec::new();
    let mut cache_stats = CacheStats::default();
    let lines = BufReader::new(file)
        .lines()
        .collect::<std::io::Result<Vec<_>>>()?;
    let total_lines = lines.len();

    for (index, line) in lines.into_iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let entry: SessionEntry = match serde_json::from_str(trimmed) {
            Ok(entry) => entry,
            Err(err) => {
                if index + 1 == total_lines {
                    eprintln!("[session] 忽略末尾截断的会话记录: {err}");
                    break;
                } else {
                    return Err(anyhow::anyhow!("会话文件第 {} 行损坏: {err}", index + 1));
                }
            }
        };
        match entry {
            SessionEntry::Message { message, usage, .. } => {
                if let Some(u) = usage {
                    cache_stats.record_usage(&u);
                }
                messages.push(message);
            }
            SessionEntry::Compaction {
                active_messages, ..
            } => messages = active_messages,
            SessionEntry::Reset { .. } => {
                messages.clear();
                cache_stats = CacheStats::default();
            }
            SessionEntry::Header { .. } => {}
        }
    }
    Ok((messages, cache_stats))
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_file() -> PathBuf {
        let count = TEST_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        env::temp_dir().join(format!(
            "rico-session-{}-{}-{count}.jsonl",
            std::process::id(),
            now_nanos()
        ))
    }

    #[test]
    fn workspace_keys_are_stable_and_distinct() {
        assert_eq!(
            workspace_key(Path::new("/tmp/a")),
            workspace_key(Path::new("/tmp/a"))
        );
        assert_ne!(
            workspace_key(Path::new("/tmp/a")),
            workspace_key(Path::new("/tmp/b"))
        );
    }

    #[test]
    fn compaction_snapshot_restores_active_context() {
        let path = temp_file();
        let mut store = SessionStore {
            path: Some(path.clone()),
            cache_stats: CacheStats::default(),
        };
        let system = Message::text("system", "system");
        let old = Message::text("user", "old");
        store.append_message(&system).unwrap();
        store.append_message(&old).unwrap();
        let active = vec![system, Message::text("system", "summary")];
        store
            .append_compaction(100, "summary".to_owned(), &active)
            .unwrap();
        store.append_message(&Message::text("user", "new")).unwrap();

        let (restored, _) = load_messages(&path).unwrap();
        assert_eq!(restored.len(), 3);
        assert_eq!(restored[1].content.as_deref(), Some("summary"));
        assert_eq!(restored[2].content.as_deref(), Some("new"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn ignores_corrupted_trailing_line() {
        let path = temp_file();
        let mut store = SessionStore {
            path: Some(path.clone()),
            cache_stats: CacheStats::default(),
        };
        let system = Message::text("system", "system");
        store.append_message(&system).unwrap();

        // 模拟进程被强杀写入了半截残缺 JSON
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(
            file,
            r#"{{"type":"message","timestamp":12345,"message":{{"role":"user""#
        )
        .unwrap();

        let (restored, _) = load_messages(&path).unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].content.as_deref(), Some("system"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn cache_stats_records_and_restores_from_file() {
        let path = temp_file();
        let mut store = SessionStore {
            path: Some(path.clone()),
            cache_stats: CacheStats::default(),
        };

        let system = Message::text("system", "prompt");
        store.append_message(&system).unwrap();

        let assistant = Message::text("assistant", "answer");
        let usage = TokenUsage {
            prompt_tokens: 1000,
            completion_tokens: 50,
            total_tokens: 1050,
            cached_tokens: 800,
        };
        store
            .append_message_with_usage(&assistant, Some(usage))
            .unwrap();

        assert_eq!(store.cache_stats().total_prompt_tokens, 1000);
        assert_eq!(store.cache_stats().total_cached_tokens, 800);
        assert_eq!(store.cache_stats().requests_count, 1);
        assert_eq!(store.cache_stats().cache_hit_requests_count, 1);
        assert_eq!(store.cache_stats().overall_hit_rate(), 80.0);

        // 验证重新从文件加载能够完整恢复统计数据
        let (messages, loaded_stats) = load_messages(&path).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(loaded_stats.total_prompt_tokens, 1000);
        assert_eq!(loaded_stats.total_cached_tokens, 800);
        assert_eq!(loaded_stats.overall_hit_rate(), 80.0);

        let _ = fs::remove_file(path);
    }
}
