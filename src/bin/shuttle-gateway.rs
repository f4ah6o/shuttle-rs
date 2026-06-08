use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "shuttle-gateway")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Serve {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        addr: Option<SocketAddr>,
        #[arg(long, default_value = "stl")]
        stl: PathBuf,
        #[arg(long, default_value = "10")]
        timeout: u64,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Serve {
            config,
            addr,
            stl,
            timeout,
        } => {
            let cfg = shuttle_rs::gateway::GatewayConfig::load(&config)
                .with_context(|| format!("failed to load {}", config.display()))?;
            if addr.is_some() && cfg.listeners.len() > 1 {
                anyhow::bail!("--addr cannot override a config with multiple listeners");
            }
            let explicit_addr = addr;
            let runtime = tokio::runtime::Runtime::new()?;
            let mut listeners = shuttle_rs::gateway::GatewayRuntime::listeners_from_config(
                cfg,
                stl,
                Duration::from_secs(timeout),
            )?;
            if let Some(addr) = explicit_addr {
                if let Some(listener) = listeners.first_mut() {
                    listener.addr = addr;
                }
            }
            runtime.block_on(shuttle_rs::gateway::serve_listeners(listeners))?;
        }
    }
    Ok(())
}
