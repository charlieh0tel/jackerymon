//! `jackerymon`: monitor a Jackery Explorer directly over BLE.

mod connection;
mod daemon;
mod daemon_client;
mod key;
mod poll;
mod record;
mod runtime;
mod scan;
mod set;
mod tui;

use std::env;
use std::io::Write;
use std::process;

use anyhow::Result;
use clap::Parser;
use clap::Subcommand;
use env_logger::Env;
use log::Level;

use crate::daemon::DaemonArgs;
use crate::daemon::log::CompressArgs;
use crate::key::KeyArgs;
use crate::poll::PollArgs;
use crate::scan::ScanArgs;
use crate::set::SetArgs;
use crate::tui::TuiArgs;

/// Set by systemd when stderr is connected to the journal.
const JOURNAL_STREAM: &str = "JOURNAL_STREAM";

/// syslog priority of a log level, as `sd-daemon(3)` prefixes take it.
fn journal_priority(level: Level) -> u8 {
    const ERROR: u8 = 3;
    const WARNING: u8 = 4;
    const INFO: u8 = 6;
    const DEBUG: u8 = 7;
    match level {
        Level::Error => ERROR,
        Level::Warn => WARNING,
        Level::Info => INFO,
        Level::Debug | Level::Trace => DEBUG,
    }
}

/// Log filter unless `RUST_LOG` overrides it: the library's notes, such as
/// recovering the unit from a stale BlueZ object.
const DEFAULT_LOG_FILTER: &str = "jackerymon=info";

#[derive(Parser)]
#[command(name = "jackerymon", about, version)]
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
    /// Fetch a unit's BLE key from Jackery's cloud, once; prompts for the
    /// account password.
    Key(KeyArgs),
    /// Live status screen; retries until the unit is reachable.
    Tui(TuiArgs),
    /// Change one setting and confirm it with a status read.
    Set(SetArgs),
    /// Gzip a daemon's finished daily logs; today's is left alone.
    CompressLogs(CompressArgs),
    /// Own the BLE connection; serve status, metrics, live updates, and
    /// settings over HTTP.
    Daemon(DaemonArgs),
}

#[tokio::main]
async fn main() -> Result<()> {
    let command = Cli::parse().command;
    // Under systemd, stderr goes to the journal, which stamps the time
    // itself and reads a leading `<N>` as the line's priority.
    let journal = env::var_os(JOURNAL_STREAM).is_some();
    // Log lines on stderr would corrupt the TUI's screen.
    if !matches!(command, Command::Tui(_)) {
        let mut logger =
            env_logger::Builder::from_env(Env::default().default_filter_or(DEFAULT_LOG_FILTER));
        if journal {
            logger.format(|buffer, record| {
                writeln!(
                    buffer,
                    "<{}>{}",
                    journal_priority(record.level()),
                    record.args()
                )
            });
        }
        logger.init();
    }
    let result = run(command).await;
    if journal && let Err(error) = &result {
        log::error!("{error:#}");
        process::exit(1);
    }
    result
}

async fn run(command: Command) -> Result<()> {
    match command {
        Command::Poll(arguments) => poll::run(arguments).await,
        Command::Key(arguments) => key::run(arguments).await,
        Command::Scan(arguments) => scan::run(arguments).await,
        Command::Tui(arguments) => tui::run(arguments).await,
        Command::Set(arguments) => set::run(arguments).await,
        Command::Daemon(arguments) => daemon::run(arguments).await,
        Command::CompressLogs(arguments) => daemon::log::compress_logs(&arguments),
    }
}
