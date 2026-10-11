//! `jackerymon`: monitor a Jackery Explorer directly over BLE.

mod connection;
mod poll;

use anyhow::Result;
use clap::Parser;
use clap::Subcommand;

use crate::poll::PollArgs;

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
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Poll(arguments) => poll::run(arguments).await,
    }
}
