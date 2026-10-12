//! `jackerymon`: monitor a Jackery Explorer directly over BLE.

mod connection;
mod daemon;
mod poll;
mod record;
mod scan;
mod set;
mod tui;

use anyhow::Result;
use clap::Parser;
use clap::Subcommand;
use env_logger::Env;

use crate::daemon::DaemonArgs;
use crate::poll::PollArgs;
use crate::scan::ScanArgs;
use crate::set::SetArgs;
use crate::tui::TuiArgs;

/// Log filter unless `RUST_LOG` overrides it: the library's notes, such as
/// recovering the unit from a stale BlueZ object.
const DEFAULT_LOG_FILTER: &str = "jackerymon=info";

#[derive(Parser)]
#[command(about, version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Poll status on one connection; JSON lines on stdout, diagnostics on
    /// stderr.
    Poll(PollArgs),
    /// List the Jackery units in range, as JSON lines; no connection.
    Scan(ScanArgs),
    /// Live status screen; retries until the unit is reachable.
    Tui(TuiArgs),
    /// Change one setting and confirm it with a status read.
    Set(SetArgs),
    /// Own the BLE connection; serve status, metrics, live updates, and
    /// settings over HTTP.
    Daemon(DaemonArgs),
}

#[tokio::main]
async fn main() -> Result<()> {
    let command = Cli::parse().command;
    // Log lines on stderr would corrupt the TUI's screen.
    if !matches!(command, Command::Tui(_)) {
        env_logger::Builder::from_env(Env::default().default_filter_or(DEFAULT_LOG_FILTER)).init();
    }
    match command {
        Command::Poll(arguments) => poll::run(arguments).await,
        Command::Scan(arguments) => scan::run(arguments).await,
        Command::Tui(arguments) => tui::run(arguments).await,
        Command::Set(arguments) => set::run(arguments).await,
        Command::Daemon(arguments) => daemon::run(arguments).await,
    }
}
