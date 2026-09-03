use std::{
    env,
    io::{self, Write},
    path::PathBuf,
};

use anyhow::{Context, Result};

mod agent;
mod auth;
mod markdown;
mod provider;
mod session;
mod tools;
mod tui;

use agent::Agent;
use auth::{auth_path, AuthStore};
use provider::OpenAiProvider;

#[tokio::main]
async fn main() -> Result<()> {
    let raw_args = env::args().skip(1).collect::<Vec<_>>();
    if raw_args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        return Ok(());
    }
    if raw_args.iter().any(|arg| arg == "--version" || arg == "-v") {
        println!("rico {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    load_config()?;

    if raw_args.first().is_some_and(|arg| arg == "auth") {
        return run_auth_command(&raw_args[1..]);
    }

    // 选择运行模式:
    //   `--cli` / `-C`  → 保持原有的 stdin/stdout REPL
    //   `--tui` / `-t`  → 显式进入 TUI (默认)
    let cli_mode = raw_args.iter().any(|arg| arg == "--cli" || arg == "-C");
    let arguments: Vec<String> = raw_args
        .into_iter()
        .filter(|arg| arg != "--cli" && arg != "-C" && arg != "--tui" && arg != "-t")
        .collect();

    let resume = arguments
        .first()
        .is_some_and(|argument| argument == "--continue" || argument == "-c");
    let arguments: Vec<String> = if resume {
        arguments.into_iter().skip(1).collect()
    } else {
        arguments
    };
    let initial_task = arguments.join(" ");

    let auth = AuthStore::load(auth_path()?)?;
    let mut providers = Vec::new();
    if let Some(api_key) = auth
        .api_key("minimax")
        .or_else(|| provider_key("MINIMAX_API_KEY", Some("OPENAI_API_KEY")))
    {
        let base_url = provider_env("MINIMAX_BASE_URL", "OPENAI_BASE_URL")
            .unwrap_or_else(|| "https://api.minimaxi.com/v1".to_owned());
        let model = provider_env("MINIMAX_MODEL", "OPENAI_MODEL")
            .unwrap_or_else(|| "MiniMax-M3".to_owned());
        providers.push(OpenAiProvider::named("minimax", api_key, base_url, model));
    }
    if let Some(api_key) = auth
        .api_key("9router")
        .or_else(|| provider_key("ROUTER_API_KEY", None))
    {
        let base_url = env::var("ROUTER_BASE_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "http://localhost:20128/v1".to_owned());
        let model = env::var("ROUTER_MODEL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "kr/claude-sonnet-4.5".to_owned());
        providers.push(OpenAiProvider::named("9router", api_key, base_url, model));
    }
    let requested_provider = env::var("RICO_PROVIDER").ok();
    let active_provider = select_active_provider(&providers, requested_provider.as_deref())?;
    let exa_api_key = auth
        .api_key("exa")
        .or_else(|| provider_key("EXA_API_KEY", None));
    env::remove_var("MINIMAX_API_KEY");
    env::remove_var("ROUTER_API_KEY");
    env::remove_var("OPENAI_API_KEY");
    env::remove_var("EXA_API_KEY");
    let max_steps = match env::var("AGENT_MAX_STEPS") {
        Ok(val) => {
            let trimmed = val.trim();
            if trimmed.eq_ignore_ascii_case("0")
                || trimmed.eq_ignore_ascii_case("none")
                || trimmed.eq_ignore_ascii_case("unlimited")
                || trimmed.is_empty()
            {
                None
            } else {
                let n = trimmed
                    .parse::<usize>()
                    .context("AGENT_MAX_STEPS 必须是正整数，或设为 0/none 表示无限制")?;
                Some(n)
            }
        }
        Err(_) => None,
    };
    let workspace = env::current_dir()?.canonicalize()?;

    let agent = Agent::new_with_providers(
        providers,
        active_provider,
        workspace,
        max_steps,
        resume,
        exa_api_key,
    )?;

    if cli_mode {
        run_cli(agent, initial_task).await
    } else {
        tui::run(agent).await
    }
}

async fn run_cli(mut agent: Agent, initial_task: String) -> Result<()> {
    println!(
        "rico coding agent（/provider 切换模型服务，/clear 清空，/session 查看会话，/cache 查看缓存，/exit 退出）"
    );
    if let Some(path) = agent.session_path() {
        println!("session: {}", path.display());
    }
    if !initial_task.trim().is_empty() {
        run_turn(&mut agent, initial_task).await;
    }

    loop {
        print!("\n你> ");
        io::stdout().flush()?;

        let mut input = String::new();
        if io::stdin().read_line(&mut input)? == 0 {
            break;
        }
        let input = input.trim();
        match input {
            "" => continue,
            "/exit" | "/quit" => break,
            "/clear" => {
                agent.clear_history()?;
                println!("上下文已清空。");
            }
            "/cache" | "/stats" => {
                println!("{}", agent.cache_stats().summary_text());
            }
            "/providers" => {
                println!(
                    "当前 provider: {}（{}）\n可用 provider: {}",
                    agent.provider_name(),
                    agent.model_name(),
                    agent.provider_names().join(", ")
                );
            }
            "/provider" => match agent.switch_provider(None) {
                Ok((provider, model)) => println!("已切换到 {provider}（模型 {model}）"),
                Err(error) => eprintln!("错误> {error:#}"),
            },
            "/session" => match agent.session_path() {
                Some(path) => {
                    println!("会话文件: {}", path.display());
                    let stats = agent.cache_stats();
                    if stats.requests_count > 0 {
                        println!("{}", stats.summary_text());
                    }
                }
                None => println!("当前为临时会话"),
            },
            _ if input.starts_with("/provider ") => {
                let requested = input.trim_start_matches("/provider ").trim();
                match agent.switch_provider(Some(requested)) {
                    Ok((provider, model)) => println!("已切换到 {provider}（模型 {model}）"),
                    Err(error) => eprintln!("错误> {error:#}"),
                }
            }
            _ => run_turn(&mut agent, input.to_owned()).await,
        }
    }

    Ok(())
}

fn print_help() {
    println!(
        r#"rico {} - 最小化的 Rust Coding Agent

用法:
  rico [选项] [首条任务...]

选项:
  -c, --continue   恢复当前工作区最近一次会话
  -t, --tui        启动 TUI 交互界面 (默认)
  -C, --cli        使用 stdin/stdout 传统 REPL
  -h, --help       显示帮助信息
  -v, --version    显示版本信息

认证:
  rico auth import 从现有环境变量/config.env 导入 API Key 到用户 auth.json
  rico auth list   查看 auth.json 中已保存的 provider（不会显示密钥）
  rico auth path   显示 auth.json 路径

TUI 快捷键:
  Enter           提交当前任务
  ↑ / ↓           浏览历史输入
  PgUp / PgDn     滚动对话历史 (Ctrl-J / Ctrl-K 同效)
  Ctrl+L          清空对话上下文
  Esc             取消当前任务；空闲时退出
  Ctrl+C          退出

CLI REPL 命令:
  /login          登录并保存 MiniMax 或 9Router API Key（TUI 中输入）
  /provider       切换到下一个 provider（可指定 9router 或 minimax）
  /providers      查看当前及可用 provider
  /clear          清空对话上下文，开始新会话
  /session        查看当前 JSONL 会话文件路径及缓存命中统计
  /cache          查询当前会话的 Prompt 缓存命中统计与命中率
  /exit, /quit    退出程序 (或按 Ctrl-D)

配置:
  可通过当前目录的 .env.local / .env、~/.config/rico/config.env 或环境变量设置:
  RICO_PROVIDER    启动 provider：minimax 或 9router（默认首个已配置项）
  MINIMAX_API_KEY  MiniMax API Key（兼容迁移；推荐存入 auth.json）
  MINIMAX_BASE_URL 默认 https://api.minimaxi.com/v1
  MINIMAX_MODEL    默认 MiniMax-M3
  ROUTER_API_KEY   9Router API Key（兼容迁移；推荐存入 auth.json）
  ROUTER_BASE_URL  默认 http://localhost:20128/v1
  ROUTER_MODEL     默认 kr/claude-sonnet-4.5
  EXA_API_KEY      Exa 搜索引擎 API Key（用于 web_search 工具；推荐存入 auth.json）
  OPENAI_*         兼容旧版 MiniMax 配置
  AGENT_MAX_STEPS  最大工具循环次数（默认无限制；可设正整数限制步数）"#,
        env!("CARGO_PKG_VERSION")
    );
}

fn run_auth_command(arguments: &[String]) -> Result<()> {
    let path = auth_path()?;
    let mut auth = AuthStore::load(path)?;
    match arguments {
        [command] if command == "import" => {
            let mut entries = Vec::new();
            if let Some(key) = provider_key("MINIMAX_API_KEY", Some("OPENAI_API_KEY")) {
                entries.push(("minimax".to_owned(), key));
            }
            if let Some(key) = provider_key("ROUTER_API_KEY", None) {
                entries.push(("9router".to_owned(), key));
            }
            if let Some(key) = provider_key("EXA_API_KEY", None) {
                entries.push(("exa".to_owned(), key));
            }
            let imported = auth.import_api_keys(entries)?;
            if imported == 0 {
                anyhow::bail!("没有找到可导入的 MINIMAX_API_KEY、ROUTER_API_KEY 或 EXA_API_KEY");
            }
            println!(
                "已将 {imported} 个 provider 凭证保存到 {}",
                auth.path().display()
            );
            println!("确认 rico 可正常启动后，可从 config.env 中删除 API Key 行。");
        }
        [command] if command == "list" => {
            let providers = auth.providers();
            if providers.is_empty() {
                println!("{} 中尚未保存认证信息", auth.path().display());
            } else {
                println!("已保存 provider: {}", providers.join(", "));
            }
        }
        [command] if command == "path" => println!("{}", auth.path().display()),
        _ => {
            println!("用法: rico auth <import|list|path>");
        }
    }
    Ok(())
}

fn load_config() -> Result<()> {
    if let Some(path) = env::var_os("RICO_CONFIG") {
        let path = PathBuf::from(path);
        return dotenvy::from_path(&path)
            .with_context(|| format!("无法读取指定的配置 {}", path.display()))
            .map(|_| ());
    }

    for local_env in [".env.local", ".env"] {
        let path = PathBuf::from(local_env);
        if path.is_file() {
            dotenvy::from_path(&path)
                .with_context(|| format!("无法读取配置 {}", path.display()))?;
        }
    }

    if let Some(home) = env::var_os("HOME") {
        let path = PathBuf::from(home).join(".config/rico/config.env");
        if path.is_file() {
            dotenvy::from_path(&path)
                .with_context(|| format!("无法读取配置 {}", path.display()))
                .map(|_| ())?;
        }
    }

    Ok(())
}

async fn run_turn(agent: &mut Agent, input: String) {
    print!("\n助手> ");
    let _ = io::stdout().flush();
    let result = agent
        .run_turn(input, |text| {
            print!("{text}");
            let _ = io::stdout().flush();
        })
        .await;
    println!();
    if let Err(error) = result {
        eprintln!("错误> {error:#}");
    }
}

fn provider_env(primary: &str, legacy: &str) -> Option<String> {
    select_provider_value(env::var(primary).ok(), env::var(legacy).ok())
}

fn select_provider_value(primary: Option<String>, legacy: Option<String>) -> Option<String> {
    primary
        .filter(|value| !value.trim().is_empty())
        .or_else(|| legacy.filter(|value| !value.trim().is_empty()))
}

fn provider_key(primary: &str, legacy: Option<&str>) -> Option<String> {
    let value = env::var(primary)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            legacy
                .and_then(|name| env::var(name).ok())
                .filter(|value| !value.trim().is_empty())
        })?;
    (!value.starts_with("your-")).then_some(value)
}

fn select_active_provider(
    providers: &[OpenAiProvider],
    requested: Option<&str>,
) -> Result<Option<usize>> {
    match requested {
        Some(name) => providers
            .iter()
            .position(|provider| provider.name().eq_ignore_ascii_case(name))
            .map(Some)
            .with_context(|| format!("RICO_PROVIDER={name} 未配置 API Key")),
        None => Ok((!providers.is_empty()).then_some(0)),
    }
}

#[cfg(test)]
mod tests {
    use super::{select_active_provider, select_provider_value};
    use crate::provider::OpenAiProvider;

    fn provider(name: &str) -> OpenAiProvider {
        OpenAiProvider::named(
            name,
            "secret".into(),
            "https://example.com/v1".into(),
            "model".into(),
        )
    }

    #[test]
    fn primary_config_takes_precedence_over_legacy_config() {
        assert_eq!(
            select_provider_value(Some("router".into()), Some("legacy".into())).as_deref(),
            Some("router")
        );
    }

    #[test]
    fn empty_primary_config_falls_back_to_legacy_config() {
        assert_eq!(
            select_provider_value(Some("  ".into()), Some("legacy".into())).as_deref(),
            Some("legacy")
        );
    }

    #[test]
    fn saved_login_becomes_active_on_next_startup() {
        let providers = vec![provider("minimax"), provider("9router")];
        assert_eq!(select_active_provider(&providers, None).unwrap(), Some(0));
    }

    #[test]
    fn requested_provider_is_selected_case_insensitively() {
        let providers = vec![provider("minimax"), provider("9router")];
        assert_eq!(
            select_active_provider(&providers, Some("9ROUTER")).unwrap(),
            Some(1)
        );
    }

    #[test]
    fn missing_requested_provider_reports_missing_key() {
        let error = select_active_provider(&[], Some("minimax")).unwrap_err();
        assert!(error.to_string().contains("未配置 API Key"));
    }
}
