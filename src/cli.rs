use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "kleviathan", about = "The sane AI orchestrator")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    #[command(about = "Run the orchestrator inside Docker (container entrypoint)")]
    RunInner {
        #[arg(long, help = "Bypass Docker container enforcement")]
        force_dangerous: bool,
        #[arg(long, help = "Bypass keyword and LLM-based abusive language checks")]
        disable_abuse_checks: bool,
    },
    #[command(about = "Build and run the Docker container on the host")]
    RunContainer,
    MakeConfig,
}
