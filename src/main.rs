#![recursion_limit = "256"]

mod cli;
mod config;
mod connectors;
mod docker;
mod engine;
mod error;
mod llm;
mod safety;

use clap::Parser;
use cli::{Cli, Commands};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer};

#[tokio::main]
async fn main() -> error::KleviathanResult<()> {
    let log_dir = std::env::var("HOME")
        .map(|home| std::path::PathBuf::from(home).join(".kleviathan").join("logs"))
        .unwrap_or_else(|_| std::path::PathBuf::from("logs"));
    std::fs::create_dir_all(&log_dir).ok();
    let log_file = std::fs::File::create(
        log_dir.join(format!(
            "kleviathan-{}.log",
            chrono::Utc::now().format("%Y%m%d-%H%M%S")
        )),
    )
    .expect("failed to create log file");

    let console_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,matrix_sdk_crypto::backups=off")),
        );

    let file_layer = fmt::layer()
        .json()
        .with_writer(std::sync::Mutex::new(log_file))
        .with_target(true)
        .with_span_events(fmt::format::FmtSpan::CLOSE)
        .with_filter(EnvFilter::new("trace,matrix_sdk_crypto::backups=off"));

    tracing_subscriber::registry()
        .with(console_layer)
        .with(file_layer)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::MakeConfig => config::make_config()?,
        Commands::RunInner { force_dangerous, disable_abuse_checks } => {
            if !force_dangerous {
                safety::container::enforce_container()?;
            }
            let config = config::load_config()?;
            engine::run(config, disable_abuse_checks).await?;
        }
        Commands::RunContainer => docker::run()?,
    }

    Ok(())
}
