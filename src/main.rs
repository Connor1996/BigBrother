use std::{
    env,
    path::PathBuf,
    sync::{atomic::AtomicBool, Arc},
};

use anyhow::Result;
use symphony_rs::{
    config::AppConfig, daemon, github::GitHubClient, model::EventLevel, runner::ShellAgentRunner,
    service::Supervisor, web,
};

#[tokio::main]
async fn main() -> Result<()> {
    let options = CliOptions::parse(env::args().skip(1).collect())?;
    let config = AppConfig::load(&options.config_path)?;
    let provider = Arc::new(GitHubClient::new(config.github.clone())?);
    let runner = Arc::new(ShellAgentRunner);
    let supervisor = Arc::new(Supervisor::new(config, provider, runner)?);

    supervisor.push_event(
        EventLevel::Info,
        None,
        format!("loaded config from {}", options.config_path.display()),
    );

    if options.headless {
        supervisor.push_event(
            EventLevel::Info,
            None,
            "running in server-only mode; --headless is now a compatibility no-op",
        );
    }

    let stop_flag = Arc::new(AtomicBool::new(false));
    let daemon_task = tokio::spawn(daemon::run_daemon(supervisor.clone(), stop_flag.clone()));

    let server_result = web::serve(
        supervisor.clone(),
        web::default_listen_addr(),
        stop_flag.clone(),
    )
    .await;

    stop_flag.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = daemon_task.await;

    server_result
}

#[derive(Debug, Clone)]
struct CliOptions {
    config_path: PathBuf,
    headless: bool,
}

impl CliOptions {
    fn parse(args: Vec<String>) -> Result<Self> {
        let mut config_path = PathBuf::from("symphony-rs.toml");
        let mut headless = false;
        let mut index = 0;

        while index < args.len() {
            match args[index].as_str() {
                "--config" => {
                    let value = args
                        .get(index + 1)
                        .ok_or_else(|| anyhow::anyhow!("--config requires a file path"))?;
                    config_path = PathBuf::from(value);
                    index += 2;
                }
                "--headless" => {
                    headless = true;
                    index += 1;
                }
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => {
                    return Err(anyhow::anyhow!("unknown argument: {other}"));
                }
            }
        }

        Ok(Self {
            config_path,
            headless,
        })
    }
}

fn print_usage() {
    println!("Usage: symphony-rs [--config <path>] [--headless]");
}
