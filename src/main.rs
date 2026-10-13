use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Utc;
use clap::Parser;
use log::info;

use rcald::{config, daemon, emit, ics, schedule};

#[derive(Parser)]
#[command(
    name = "rcald",
    version,
    about = "calendar reminder daemon: watches ics calendars and emits reminders as JSON lines on stdout"
)]
struct Args {
    /// Path to the TOML config file.
    #[arg(short, long, default_value = "./rcald.toml")]
    config: PathBuf,

    /// Increase log verbosity: default info, -v debug, -vv trace.
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Fetch calendars once, emit every reminder due until now+fetch_period, exit.
    /// Suitable for cron/systemd timer instead of the long-running loop.
    #[arg(long)]
    once: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    init_log(args.verbose);

    let cfg = config::load(&args.config)?;
    let local_tz = ics::local_tz();
    info!(
        "rcald starting (config: {}, tz: {}, {} calendars, fetch_period: {:?})",
        args.config.display(),
        local_tz.name(),
        cfg.calendars.len(),
        cfg.fetch_period
    );

    if args.once {
        run_once(&cfg, local_tz)
    } else {
        daemon::run(&cfg, local_tz)
    }
}

/// One-shot mode for cron/systemd timers: fresh fetch, emit all reminders due
/// in [past-until-now+fetch_period], exit.
fn run_once(cfg: &config::RuntimeConfig, local_tz: chrono_tz::Tz) -> Result<()> {
    let agent = ics::make_agent();
    let mut states = schedule::CalendarState::from_configs(cfg);
    daemon::refresh_all(&agent, &mut states, local_tz);

    let now = Utc::now();
    let horizon =
        now + chrono::Duration::from_std(cfg.fetch_period).context("fetch_period too large")?;
    // Empty FiredSet: every --once run re-emits everything due in the window
    // (overdue ones flagged "late"). Dedup across runs is left to downstream.
    let (due, _) = schedule::collect(&states, now, horizon, &schedule::FiredSet::new());
    let n = due.len();
    for r in &due {
        emit::emit(r);
    }
    info!("once mode: emitted {} reminder(s)", n);
    Ok(())
}

fn init_log(verbosity: u8) {
    let level = match verbosity {
        0 => log::LevelFilter::Info,
        1 => log::LevelFilter::Debug,
        _ => log::LevelFilter::Trace,
    };
    env_logger::Builder::new()
        .target(env_logger::Target::Stderr)
        .filter_level(level)
        .format_timestamp_millis()
        .init();
}
