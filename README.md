# rico

一个最小 Rust coding agent，通过 MiniMax 中国站 Token Plan 的 OpenAI 兼容接口驱动，默认使用 `MiniMax-M3`。

## 配置

编辑用户级配置 `~/.config/rico/config.env`，并将权限设为仅自己可读写：

```dotenv
OPENAI_API_KEY=你的-sk-cp-Token-Plan-Key
OPENAI_BASE_URL=https://api.minimaxi.com/v1
OPENAI_MODEL=MiniMax-M3
AGENT_MAX_STEPS=20
```

Token Plan Key 与按量付费 API Key 不互通。请在 MiniMax 中国站的“接口密钥”页面创建或复制 `sk-cp-` Key。

```bash
chmod 600 ~/.config/rico/config.env
```

也可以用 `RICO_CONFIG` 指定其他配置路径，或直接设置环境变量。密钥读取后会从 agent 子进程环境中移除。

## 多轮对话

```bash
cargo run
```

默认启动 TUI 终端界面，支持流式输出、滚动、状态栏与历史输入回溯。如果终端环境不支持 TUI，可改用传统 REPL：

```bash
cargo run -- --cli
```

### TUI 模式（默认）

界面分四块：

1. **标题栏**：模型名 + 当前状态（idle / thinking… / running tool… / compacting context…）
2. **对话区**：自动滚动的历史；工具调用以 ⚙ 标记折叠展示
3. **输入框**：Enter 提交，↑/↓ 翻历史输入
4. **状态栏**：当前 JSONL 会话路径 · 估算 token · 步数

快捷键：

| 按键 | 行为 |
| :--- | :--- |
| `Enter` | 提交任务 |
| `↑` / `↓` | 在历史输入中切换 |
| `PgUp` / `PgDn` (或 `Ctrl-J` / `Ctrl-K`) | 滚动对话历史 |
| `Ctrl+L` | 清空对话上下文（新会话） |
| `Ctrl+C` / `Esc` | 优雅退出 |

TUI 接管整屏，结束时自动恢复终端 raw mode 与 alternate screen。

### CLI 模式 (`--cli`)

保留原有的 stdin/stdout REPL，可用于管道、脚本或不支持 TUI 的环境：

```text
你> 检查这个项目的结构
助手> ...
你> 根据刚才的建议补充测试
助手> ...
```

- `/clear`：清空对话上下文，开始新会话
- `/session`：显示当前 JSONL 会话文件
- `/exit` 或 `/quit`：退出
- `Ctrl-D`：结束输入并退出

也可以将第一条消息放在命令行中，回答后仍会进入交互模式：

```bash
cargo run -- "检查这个项目并补充一个健康检查接口"
```

安装为全局命令后可在任意项目目录运行：

```bash
rico
rico "检查这个项目"
```

恢复当前项目最近一次会话：

```bash
rico --continue
rico -c "继续完成刚才的任务"
```

显式选择运行模式：

```bash
rico --tui      # 默认
rico --cli      # 传统 REPL
```

会话按工作目录保存在 `~/.local/share/rico/sessions/` 下，每条消息和压缩事件都会立即追加为一行 JSON。

模型文本采用 SSE 流式输出。上下文估算超过 `RICO_COMPACT_TOKENS`（默认 200,000）时，rico 会自动总结旧消息，并保留最近约 `RICO_KEEP_RECENT_TOKENS`（默认 20,000）的内容；完整历史仍保留在 JSONL 文件中。

## 工具

rico 采用 Pi 风格的动态工具注册表，默认向模型提供：

- `read`：分页读取文本文件
- `write`：创建或完整覆盖文件
- `edit`：精确替换唯一匹配文本
- `bash`：在工作目录中执行完整 shell 命令，包括常规网络命令

文件工具拒绝密钥文件、凭据目录和符号链接路径。`bash` 默认超时 120 秒，可由模型设置为最长 600 秒；标准输出和错误输出在读取过程中即实施有界采集，最终保留末尾最多 50KB 或 2,000 行。

MiniMax Key 存放在工作区外，读取后会从 rico 进程环境中移除，因此不会作为环境变量传给 shell。不过 `bash` 与当前用户拥有相同的系统权限，应只在可信项目中使用。

## License

[MIT](LICENSE)
