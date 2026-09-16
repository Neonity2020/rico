# rico 项目评估快照

> 更新时间：2026-09-12。此文件记录当前可重复验证的状态，不替代 CI 结果。

## 当前结论

rico 是结构清晰的 Rust coding agent，具备 OpenAI 兼容流式协议、五个内置工具（`read` / `write` / `edit` / `bash` / `web_search`）、TUI/CLI、JSONL 会话恢复和 Prompt Cache 指标。它适合作为个人或可信项目中的本地工具；`bash` 具有当前用户权限，因此不应描述为操作系统级安全沙箱。

## 自动化验证

| 检查 | 当前结果 |
| :--- | :--- |
| `cargo fmt --all -- --check` | 通过 |
| `cargo clippy --all-targets --all-features -- -D warnings` | 通过，零警告 |
| `cargo test --all-targets` | 70 passed |
| `cargo audit` | 依赖 212 个（Cargo.lock）；本地未安装 cargo-audit，漏洞扫描由 CI 的 rustsec/audit-check 在每次推送时执行 |
| `npm audit --omit=dev`（marketing） | 0 个已知漏洞 |
| `npm run build`（marketing） | 通过（要求 Node ≥ 22.12.0，CI 使用 22.12.0） |

## 已落实的风险收敛

- shell stdout/stderr 改为有界流式采集，避免截断前无限累积内存。
- SSE 必须收到 `[DONE]` 或非空 `finish_reason`，否则按不完整响应报错。
- 修复最大工具步数错误在 TUI 中重复展示的问题。
- 配置文件只补充缺失环境变量，允许环境密钥与文件中的 provider/model 配置组合使用。
- 营销站明确 `bash` 的完整用户权限，不再使用“严格隔离沙箱”或无基准数据的性能承诺。
- 达到最大工具步数（`AGENT_MAX_STEPS`）时执行优雅收尾总结轮：禁用工具定义并引导模型汇报阶段性成果，替代原先直接报错中断，消息历史保持 `user → tool → assistant` 闭环。
- 移除从未生效的 `stty` PTY 尺寸对账死代码（`Command::output()` 不继承 stdin，画布尺寸由 crossterm 的 Resize 事件承担），同时消除帧循环中每秒一次的子进程开销。
- `CODE_GUIDE.md` 与实现逐条对齐：Agent 结构体（多 provider、`max_steps: Option<usize>`、取消标记）、`AgentEvent` 变体（`Usage`、`Step.total: Option<usize>`）、max_steps 错误路径、TUI 四区布局、网络出口（模型请求 + Exa 检索）、敏感文件黑名单与 CLI REPL 指令清单。

## 后续重点

- 引入可替换的 provider 接口和本地 mock HTTP server，为重试、截断 SSE、工具循环与压缩流程增加端到端测试。
- 若需要处理不可信仓库，应增加默认关闭 `bash` 的安全模式，或使用容器/系统沙箱执行命令。
- `read` 工具在分页前将整个文件读入内存，缺少文件大小上限，应加防守权限。
- TUI 会话区在任一 agent 事件后会整体失效并全量重渲染，长会话高频流式输出时性能退化，应考虑增量渲染。
- 发布站点确定正式域名后补充 canonical、`og:url` 和社交分享图片。
