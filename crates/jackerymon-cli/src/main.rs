//! `jackerymon`: monitor a Jackery Explorer directly over BLE.

mod connection;
mod poll;
mod tui;

use anyhow::Result;
use clap::Parser;
use clap::Subcommand;

use crate::poll::PollArgs;
use crate::tui::TuiArgs;

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
    /// Live status screen; retries until the unit is reachable.
    Tui(TuiArgs),
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Poll(arguments) => poll::run(arguments).await,
        Command::Tui(arguments) => tui::run(arguments).await,
    }
}
