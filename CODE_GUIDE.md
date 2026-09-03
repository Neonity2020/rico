# rico 代码导读

`rico` 是一个最小化的 Rust coding agent。它通过 OpenAI 兼容的 Chat Completions 流式接口（默认指向 MiniMax M3）驱动模型，并提供四个面向编程的核心工具（`read`, `write`, `edit`, `bash`），支持多轮会话持久化与长对话上下文自动压缩。

默认启动一个 ratatui TUI（终端 UI），同时保留传统的 stdin/stdout REPL 作为 `--cli` 兼容入口。

本文档面向想要阅读或扩展本项目的开发者，逐文件说明模块职责、关键流程与安全边界。

---

## 1. 项目结构

```
.
├── Cargo.toml          # 依赖：anyhow / dotenvy / reqwest(rustls) / serde / tokio / ratatui / crossterm
├── .env.example        # 环境变量模板
├── README.md           # 使用说明与快速上手
├── CODE_GUIDE.md       # 架构导读与安全设计（本文档）
└── src
    ├── main.rs         # 入口：CLI 解析 (--tui/--cli)、配置加载、CLI REPL 循环驱动
    ├── agent.rs        # Agent 核心：提示词、工具调度循环、上下文自动压缩、事件通道
    ├── provider.rs     # Provider：OpenAI 兼容 SSE 流式请求与 tool_calls、Token Usage 解析
    ├── session.rs      # 会话存储：基于 JSONL 的追加存储、缓存命中统计与会话恢复
    ├── tools.rs        # 工具注册表：受控文件工具、完整 shell、超时与输出限制
    ├── markdown/       # Markdown 解析与渲染文件组
    │   ├── mod.rs      # 统一入口与各块级元素协调 (render)
    │   ├── code.rs     # 代码块词法高亮 (highlight_code) 与等宽框线闭合
    │   ├── table.rs    # GFM 管道表格解析、CJK 全角对齐与截断填充
    │   └── inline.rs   # 行内格式（粗体/斜体/代码）与段落折行
    └── tui/            # TUI 交互前端文件组
        ├── mod.rs      # 终端生命周期控制与主事件循环 (run)
        ├── app.rs      # 核心状态模型 (App)、事件更新与指令拦截
        ├── event.rs    # 键盘按键分发与鼠标交互
        ├── editor.rs   # Unicode 字符编辑、光标几何位置与折行换算
        ├── view.rs     # 对话气泡、欢迎横幅、状态栏与底栏视图渲染
        └── selection.rs# 选区高亮计算与系统剪贴板集成 (pbcopy / OSC 52)
```

保持轻量无框架设计：无外部 LLM SDK，直接基于 `reqwest` + `tokio` + `serde_json` 实现；TUI 同样基于 `ratatui` + `crossterm` 直接构造，采用高内聚、低耦合的文件组结构组织，无过度抽象。

---

## 2. 启动与配置流程（`src/main.rs`）

1. **CLI 快速分支**：
   - `--help` / `-h`：输出帮助说明并退出，不发起任何模型调用或网络请求。
   - `--version` / `-v`：输出当前 Cargo 包版本并退出。
   - `--continue` / `-c`：标记 `resume = true`，用于恢复当前工作区的最新会话。
   - `--tui` / `-t`（默认）与 `--cli` / `-C`：选择运行模式。
   - 剩余参数被拼装为 `initial_task`，作为启动后的首轮任务执行。
2. **多级配置加载（`load_config`）**：
   - 现有环境变量拥有最高优先级，dotenv 加载不会覆盖它们。
   - 若指定 `RICO_CONFIG`，仅从该文件补充缺失配置。
   - 否则依次从当前目录的 `.env.local`、`.env` 和用户级 `~/.config/rico/config.env` 补充缺失配置。
3. **敏感凭证防泄漏**：
   - 验证 `OPENAI_API_KEY` 有效性（拒绝 `your-` 占位符）。
   - **`env::remove_var("OPENAI_API_KEY")`**：读取后立即从当前进程环境变量中抹除，防止随后续的 `bash` 子进程环境暴露给未知命令。
4. **运行模式分派**：
   - `--cli`：调用 `run_cli(agent, initial_task)`，维持原 stdin/stdout REPL。
   - 默认 / `--tui`：调用 `tui::run(agent)`，进入全屏 TUI。
5. **CLI REPL 特殊指令**：
   - `/clear`：重置对话上下文，开启新会话。
   - `/session`：查看当前保存的 JSONL 文件绝对路径。
   - `/exit` / `/quit` / `Ctrl-D`：优雅退出。

---

## 3. 对话与执行循环（`src/agent.rs`）

`Agent` 维护运行时状态：

```rust
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
```

### 回合执行逻辑（`run_turn`）

```text
1. maybe_compact() -> 检查 token 估算，必要时自动向模型请求生成摘要并压缩旧消息
2. push user message -> 追加到 session.jsonl
3. step in 1..=max_steps:
    a. emit AgentEvent::Step { current, total }
    b. provider.chat_stream(messages, tool_definitions, on_text)
       -> 每段文本都通过 on_text 闭包转发给调用方
    c. push assistant message (包含 text 与 tool_calls)
    d. 若无 tool_calls:
        - emit AgentEvent::Complete
        - 返回最终回答文本
    e. 若有 tool_calls:
        - emit AgentEvent::ToolStart { name, args }
        - 遍历每一个 call，通过 tools.execute(name, arguments) 执行
        - emit AgentEvent::ToolResult { output }
        - push tool 结果消息 (role="tool", tool_call_id=call.id)
4. 超过 max_steps 时 emit AgentEvent::Error 并返回限制提示
```

### 上下文自动压缩（`maybe_compact`）

- **触发条件**：当前历史 token 估算超过 `RICO_COMPACT_TOKENS`（默认 200,000）。
- **保留窗口**：逆向扫描历史，保留最近约 `RICO_KEEP_RECENT_TOKENS`（默认 20,000）的消息，并对齐到合法的 `user` 角色边界。
- **容错降级**：若请求模型总结失败（如网络波动），自动打印警告日志并跳过当前压缩，确保本轮对话流程绝不中断。
- **Token 估算（`estimate_tokens`）**：对中文字符（非 ASCII）赋予合理权重（约 1.25 tokens/字），避免中文语境下低估 Token 导致超限溢出。

### AgentEvent 与观察者（`event_sink`）

`AgentEvent` 是 Agent 在回合过程中发出的事件集合：

```rust
pub enum AgentEvent {
    Delta(String),
    ToolStart { name: String, args: String },
    ToolResult { output: String },
    Step { current: usize, total: usize },
    Compacting,
    Complete,
    Error(String),
}
```

`set_event_sink` 允许调用方注入一个 `FnMut(AgentEvent) + Send` 闭包：
- CLI REPL 模式下不设置（默认 `None`），所有事件被丢弃，仅靠 `on_text` 打印流式文本。
- TUI 模式下设置为一个把事件转发到 `tokio::sync::mpsc::UnboundedSender` 的闭包，驱动渲染循环。

---

## 4. Provider 与流式协议（`src/provider.rs`）

### SSE 流式传输与事件重建

- 构造 `POST {base_url}/chat/completions`，开启 `stream: true`。
- 不设全局总响应超时，保留连接超时，保证长思考或长代码生成不被强制掐断。
- **`find_event_end`**：精确定位 `\n\n` 或 `\r\n\r\n` 分隔符，以字节级缓冲消费完整的 SSE 事件块。
- **`tool_calls` 去重累加**：针对部分兼容代理在每个 chunk 中重复发送完整 `name` 和 `id` 的行为，`process_event` 确保 `id` 和 `name` 仅在初次或有效更新时赋值，避免 `"bashbash"` 重复拼接，同时对 `arguments` 进行增量 `push_str`。

---

## 5. 会话持久化与恢复（`src/session.rs`）

- **存储路径**：默认存放在 `~/.local/share/rico/sessions/{workspace_key}/` 下。
- **稳定工作区指纹（`workspace_key`）**：采用确定性的 64 位 **FNV-1a** 哈希算法，保证跨平台、跨 Rust 编译器升级的目录唯一与一致性。
- **JSONL 追加格式**：
  - `header`：版本号与创建时间戳。
  - `message`：即时落盘的消息。
  - `compaction`：记录压缩前 token 数、生成的摘要及活跃上下文快照。
  - `reset`：`/clear` 事件标记。
- **断电与崩溃容错**：`load_messages` 若遇文件最后一行被意外强杀导致的残缺 JSON，自动打印警告并跳过尾行，保证历史会话平稳恢复。

---

## 6. 工具箱与信任边界（`src/tools.rs`）

rico 默认注册四个面向编码的工具：

| 工具名 | 核心功能 | 安全边界与特性 |
| :--- | :--- | :--- |
| **`read`** | 分页读取工作区文本文件 | 支持 1-based 行偏移与行数限制；限制单行最长 2,000 字符 |
| **`write`** | 创建或覆盖工作区文件 | 自动递归创建父目录 |
| **`edit`** | 精确替换文本 | 要求 `old_text` 在目标文件中恰好出现 1 次，杜绝歧义替换 |
| **`bash`** | 执行 Shell 命令与测试 | 超时控制（1~600 秒）；Unix 下创建进程组，超时彻底清理进程树 |

### 安全防护机制

1. **工作区目录穿透防御（`Workspace::path`）**：
   - 拒绝绝对路径、包含 `..` 的路径及 Windows 盘符。
   - 逐级检查路径各分量，拒绝任何符号链接（Symlink），防止指向外部敏感文件。
2. **敏感凭据保护（`is_sensitive_path`）**：
   - 拦截 `.env*`、`.aws`、`.ssh`、`id_rsa`、`id_ed25519`、`id_ecdsa`、`id_dsa`、`config.env`、`*.key`、`*.pem` 等凭据文件，防止文件工具意外泄露密钥。
3. **孤儿进程清理**：
   - `BashTool` 在 Unix 系统下设置 `command.process_group(0)`。超时触发时，向整个进程组发送 `SIGKILL`，杜绝后台死循环脚本或失控子进程残留。
4. **有界输出采集（`read_bounded_tail` + `truncate_tail`）**：
   - stdout/stderr 在读取阶段分别只保留 50 KiB 尾部，避免高输出命令先耗尽进程内存；合并后再限制为 2,000 行或 50 KiB。

这些限制只适用于 `read`、`write`、`edit`。`bash` 是当前用户权限下的完整 shell，可以访问工作区外文件和网络，不构成操作系统级沙箱，只应在可信项目中启用。

---

## 7. TUI 前端（`src/tui/`）

### 进程拓扑

```text
main 启动
   ├── agent = Agent::new(...)
   ├── event_tx, event_rx = mpsc::unbounded_channel::<AgentEvent>()
   ├── cmd_tx,   cmd_rx   = mpsc::unbounded_channel::<UserCommand>()
   ├── agent.set_event_sink(|event| event_tx.send(event))
   └── tokio::spawn(run_agent(agent, cmd_rx, event_tx.clone()))
                       ▲                        │
                       │                        ▼
                  agent 任务持有 cmd_rx     主线程持有 event_rx
                  (在后台处理请求)          (驱动 ratatui 渲染)
```

主线程 `tokio::select!` 同时监听：

- `crossterm::event::EventStream`：按键（Enter / Esc / Ctrl+L / Ctrl-C / ↑/↓ / PgUp/PgDn / 字符输入等）
- `event_rx`：Agent 推送的 `AgentEvent`
- `tokio::time::sleep(80ms)`：节流重绘，保证流式输出视觉连贯

### 终端初始化与清理

- 进入：开启 raw mode、alternate screen、mouse capture。
- 退出：通过 `scopeguard::defer!` 在任何路径（包括 panic）下都会调用 `restore_terminal()`，避免留下坏掉的终端状态。

### 布局（ratatui）

```text
┌──────────────────────────────────────────┐
│  rico · MiniMax-M3 · idle                │  ← Header (3 行)
├──────────────────────────────────────────┤
│  你> ...                                 │
│  助手> ...                               │  ← History (自适应)
│  ⚙ bash: ls -la                         │     (支持滚动)
│    ✓ → ...                               │
├──────────────────────────────────────────┤
│ > _                                     │  ← Input (3 行)
├──────────────────────────────────────────┤
│ session: .../foo.jsonl · 1.2k tokens    │  ← Status (1 行)
└──────────────────────────────────────────┘
```

- 流式 `AgentEvent::Delta` 累加到 `streaming: Option<Entry>`，遇 `Complete` / `Error` / 下一条用户消息时 flush 到 `entries`。
- `Ctrl+L` 发送 `UserCommand::Reset`，agent 落盘 `Reset` 事件并清空上下文。
- `↑` / `↓` 在历史输入中切换（仅非 busy 时）；`PgUp` / `PgDn` / `Ctrl-J` / `Ctrl-K` 滚动对话区。
- 历史区自动跟随新内容（`autoscroll`），用户滚动后停止自动跟随；`PgDn` 至底部时恢复。

### 安全一致性

- TUI 模式与 CLI 模式共用同一个 `Agent`，因此 **文件工具路径防护、敏感文件黑名单、API Key 环境变量移除、超时、输出限制与进程组清理** 等行为保持一致。
- TUI 不会调用任何额外 shell 或网络命令；唯一网络路径仍由 `provider.rs` 控制。

---

## 8. 验证与测试

```bash
cargo check                        # 验证编译
cargo clippy --all-targets         # 代码质量检查 (当前零警告)
cargo test                         # 运行单元测试套件 (17 个测试)
cargo run -- --help                # 验证 CLI 帮助
cargo run -- --cli                 # CLI 模式交互 (需要 OPENAI_API_KEY)
cargo run -- --tui                 # TUI 模式交互 (默认)
```
