mod calendar;
mod config;
mod daemon;
mod ics;
mod output;
mod state;

use std::{
    env,
    io::{self, IsTerminal},
    path::PathBuf,
    process::ExitCode,
};

use clap::Parser;
use tracing::{Level, error};

use crate::{config::Config, daemon::Daemon};

/// Watches iCalendar feeds and prints due alarms as JSON lines on stdout.
///
/// Signals: SIGHUP reloads the config, SIGUSR1 fetches now, SIGTERM/SIGINT exit.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Config file [default: $XDG_CONFIG_HOME/rcald/config.toml]
    #[arg(short, long)]
    config: Option<PathBuf>,
    /// File remembering emitted alarms [default: $XDG_STATE_HOME/rcald/state.json]
    #[arg(long)]
    state: Option<PathBuf>,
    /// Fetch once, print the N nearest events and exit
    #[arg(long, value_name = "N", num_args = 0..=1, default_missing_value = "10")]
    once: Option<usize>,
    /// Log debug messages to stderr
    #[arg(short, long)]
    verbose: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .with_max_level(if cli.verbose { Level::DEBUG } else { Level::INFO })
        .init();

    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        // The consumer went away; nothing left to do.
        Err(e) if e.downcast_ref::<io::Error>().is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe) => {
            ExitCode::SUCCESS
        }
        Err(e) => {
            error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    let config_path = cli.config.unwrap_or_else(|| xdg_path("XDG_CONFIG_HOME", ".config", "config.toml"));
    let config = Config::load(&config_path)?;
    match cli.once {
        Some(n) => daemon::once(&config, n).await,
        None => {
            let state_path = cli.state.unwrap_or_else(|| xdg_path("XDG_STATE_HOME", ".local/state", "state.json"));
            Daemon::new(config_path, config, state_path)?.run().await
        }
    }
}

fn xdg_path(var: &str, home_fallback: &str, file: &str) -> PathBuf {
    let base = env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(home_fallback));
    base.join(env!("CARGO_PKG_NAME")).join(file)
}
