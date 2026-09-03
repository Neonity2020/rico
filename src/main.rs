use std::{
    env,
    io::{self, Write},
    path::PathBuf,
};

use anyhow::{Context, Result};

mod agent;
mod markdown;
mod provider;
mod session;
mod tools;
mod tui;

use agent::Agent;
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

    // 选择运行模式:
    //   `--cli` / `-C`  → 保持原有的 stdin/stdout REPL
    //   `--tui` / `-t`  → 显式进入 TUI (默认)
    let cli_mode = raw_args
        .iter()
        .any(|arg| arg == "--cli" || arg == "-C");
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

    let api_key = required_env("OPENAI_API_KEY")?;
    if api_key.starts_with("your-") {
        anyhow::bail!("请在 rico 配置中填写真实的 MiniMax Token Plan Key（sk-cp-...）");
    }
    env::remove_var("OPENAI_API_KEY");

    let base_url =
        env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.minimaxi.com/v1".to_owned());
    let model = env::var("OPENAI_MODEL").unwrap_or_else(|_| "MiniMax-M3".to_owned());
    let max_steps = env::var("AGENT_MAX_STEPS")
        .unwrap_or_else(|_| "20".to_owned())
        .parse::<usize>()
        .context("AGENT_MAX_STEPS 必须是正整数")?;
    let workspace = env::current_dir()?.canonicalize()?;

    let provider = OpenAiProvider::new(api_key, base_url, model);
    let agent = Agent::new(provider, workspace, max_steps, resume)?;

    if cli_mode {
        run_cli(agent, initial_task).await
    } else {
        tui::run(agent).await
    }
}

async fn run_cli(mut agent: Agent, initial_task: String) -> Result<()> {
    println!("rico coding agent（/clear 清空，/session 查看会话，/cache 查看缓存，/exit 退出）");
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

TUI 快捷键:
  Enter           提交当前任务
  ↑ / ↓           浏览历史输入
  PgUp / PgDn     滚动对话历史 (Ctrl-J / Ctrl-K 同效)
  Ctrl+L          清空对话上下文
  Ctrl+C, Esc     退出

CLI REPL 命令:
  /clear          清空对话上下文，开始新会话
  /session        查看当前 JSONL 会话文件路径及缓存命中统计
  /cache          查询当前会话的 Prompt 缓存命中统计与命中率
  /exit, /quit    退出程序 (或按 Ctrl-D)

配置:
  可通过当前目录的 .env.local / .env、~/.config/rico/config.env 或环境变量设置:
  OPENAI_API_KEY   MiniMax Token Plan Key (sk-cp-...)
  OPENAI_BASE_URL  默认 https://api.minimaxi.com/v1
  OPENAI_MODEL     默认 MiniMax-M3
  AGENT_MAX_STEPS  最大工具循环次数 (默认 20)"#,
        env!("CARGO_PKG_VERSION")
    );
}

fn load_config() -> Result<()> {
    if env::var_os("OPENAI_API_KEY").is_some() {
        return Ok(());
    }

    if let Some(path) = env::var_os("RICO_CONFIG") {
        let path = PathBuf::from(path);
        return dotenvy::from_path(&path)
            .with_context(|| format!("无法读取指定的配置 {}", path.display()))
            .map(|_| ());
    }

    for local_env in [".env.local", ".env"] {
        let path = PathBuf::from(local_env);
        if path.is_file() {
            let _ = dotenvy::from_path(&path);
            if env::var_os("OPENAI_API_KEY").is_some() {
                return Ok(());
            }
        }
    }

    if let Some(home) = env::var_os("HOME") {
        let path = PathBuf::from(home).join(".config/rico/config.env");
        if path.is_file() {
            return dotenvy::from_path(&path)
                .with_context(|| format!("无法读取配置 {}", path.display()))
                .map(|_| ());
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

fn required_env(name: &str) -> Result<String> {
    env::var(name).with_context(|| format!("缺少环境变量 {name}"))
}