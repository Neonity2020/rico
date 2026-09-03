# rico

一个最小 Rust coding agent，可通过 MiniMax 或 9Router 的 OpenAI 兼容接口驱动，并可在 TUI 中运行时切换 provider。

## 配置

认证信息使用与 Pi 相同的 credential 结构，保存在 `~/.config/rico/auth.json`：

```json
{
  "minimax": {
    "type": "api_key",
    "key": "你的-MiniMax-API-Key"
  },
  "9router": {
    "type": "api_key",
    "key": "你的-9Router-API-Key"
  }
}
```

推荐从现有配置自动导入，rico 会以 `0600` 权限创建文件：

```bash
rico auth import
rico auth list
rico auth path
```

导入并确认启动正常后，从 `config.env` 删除 `MINIMAX_API_KEY`、`ROUTER_API_KEY` 和旧版 `OPENAI_API_KEY`。`auth.json` 优先于环境变量；环境变量仅用于兼容和迁移。可用 `RICO_AUTH` 指定其他认证文件路径。

非敏感设置继续放在用户级配置 `~/.config/rico/config.env`：

```dotenv
# 可选：minimax 或 9router；不设置时 MiniMax 优先
# RICO_PROVIDER=minimax

MINIMAX_BASE_URL=https://api.minimaxi.com/v1
MINIMAX_MODEL=MiniMax-M3

ROUTER_BASE_URL=http://localhost:20128/v1
ROUTER_MODEL=kr/claude-sonnet-4.5
# AGENT_MAX_STEPS=20  # 可选：默认无轮数限制
```

只需配置实际使用的 provider；同时配置两组 API Key 后即可在 TUI 内切换。`RICO_PROVIDER` 控制启动时使用的 provider，可设为 `minimax` 或 `9router`。如果未设置，则使用配置列表中的第一个 provider（MiniMax 优先）。

使用 9Router 前，先在 Dashboard 中连接上游 provider、生成 API Key，并将模型名改成已启用的模型或 combo。使用 9Router Cloud 时，将 `ROUTER_BASE_URL` 改为 `https://9router.com/v1`。

```bash
chmod 600 ~/.config/rico/config.env
```

也可以用 `RICO_CONFIG` 指定其他非敏感配置路径。旧版的 `OPENAI_BASE_URL`、`OPENAI_MODEL` 仍作为 MiniMax 配置的兼容回退。环境变量中的密钥读取后会从 agent 子进程环境中移除。

### 在 TUI 中切换 provider

```text
/providers          # 查看当前及所有已配置 provider
/provider            # 在已配置 provider 间轮换
/provider 9router    # 切换到 9Router
/provider minimax    # 切换到 MiniMax
```

切换后，状态栏和模型名称会立即更新；当前对话上下文会保留，下一次模型请求开始使用新 provider。

### 在 TUI 中登录

新用户可直接在 TUI 输入：

```text
/login minimax
/login 9router
```

随后在输入框中输入 API Key 并按 Enter。输入内容会被掩码，不会进入会话历史；成功后凭证会写入 `auth.json`，并立即启用对应 provider。按 Esc 可取消登录。

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
| `Esc` | 运行中取消当前任务；空闲时退出 |
| `Ctrl+C` / `Ctrl+D` | 退出 |

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

模型文本采用 SSE 流式输出。连接超时为 10 秒；连接建立后若连续 90 秒没有读到任何数据，会终止当前请求并报告错误，避免 provider 半开连接让任务永久卡住。运行中可按 `Esc` 立即取消；取消会把模型上下文和缓存统计回滚到本轮开始前，未完成的中间消息不会污染后续会话。上下文估算超过 `RICO_COMPACT_TOKENS`（默认 200,000）时，rico 会自动总结旧消息，并保留最近约 `RICO_KEEP_RECENT_TOKENS`（默认 20,000）的内容；完整历史仍保留在 JSONL 文件中。

## 工具

rico 采用 Pi 风格的动态工具注册表，默认向模型提供：

- `read`：分页读取文本文件
- `write`：创建或完整覆盖文件
- `edit`：精确替换唯一匹配文本
- `bash`：在工作目录中执行完整 shell 命令，包括常规网络命令

文件工具拒绝密钥文件、凭据目录和符号链接路径。`bash` 默认超时 120 秒，可由模型设置为最长 600 秒；标准输出和错误输出在读取过程中即实施有界采集，最终保留末尾最多 50KB 或 2,000 行。命令退出后若后台进程仍占用输出管道，rico 最多等待 2 秒，随后终止对应进程组并返回明确错误。

MiniMax 与 9Router Key 存放在工作区外的 `auth.json` 中；新文件权限为 `0600`，coding tools 也会拒绝访问任何名为 `auth.json` 的路径。环境变量兼容方式读取的密钥会从 rico 进程环境中移除，因此不会传给 shell。不过 `bash` 与当前用户拥有相同的系统权限，应只在可信项目中使用。

## License

[MIT](LICENSE)
