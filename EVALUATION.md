# rico 项目评估快照

> 更新时间：2026-09-03。此文件记录当前可重复验证的状态，不替代 CI 结果。

## 当前结论

rico 是结构清晰的 Rust coding agent，具备 OpenAI 兼容流式协议、四个编码工具、TUI/CLI、JSONL 会话恢复和 Prompt Cache 指标。它适合作为个人或可信项目中的本地工具；`bash` 具有当前用户权限，因此不应描述为操作系统级安全沙箱。

## 自动化验证

| 检查 | 当前结果 |
| :--- | :--- |
| `cargo fmt --all -- --check` | 通过 |
| `cargo clippy --all-targets --all-features -- -D warnings` | 通过 |
| `cargo test --all-targets` | 47 passed |
| `cargo audit` | 212 个依赖，0 个已知漏洞 |
| `npm audit --omit=dev` | 0 个已知漏洞 |
| `npm run build`（marketing） | 通过 |

## 已落实的风险收敛

- shell stdout/stderr 改为有界流式采集，避免截断前无限累积内存。
- SSE 必须收到 `[DONE]` 或非空 `finish_reason`，否则按不完整响应报错。
- 修复最大工具步数错误在 TUI 中重复展示的问题。
- 配置文件只补充缺失环境变量，允许环境密钥与文件中的 provider/model 配置组合使用。
- 营销站明确 `bash` 的完整用户权限，不再使用“严格隔离沙箱”或无基准数据的性能承诺。

## 后续重点

- 引入可替换的 provider 接口和本地 mock HTTP server，为重试、截断 SSE、工具循环与压缩流程增加端到端测试。
- 若需要处理不可信仓库，应增加默认关闭 `bash` 的安全模式，或使用容器/系统沙箱执行命令。
- 发布站点确定正式域名后补充 canonical、`og:url` 和社交分享图片。
