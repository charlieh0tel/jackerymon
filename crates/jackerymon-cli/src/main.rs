//! `jackerymon`: monitor a Jackery Explorer directly over BLE.

use std::num::NonZeroU32;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use chrono::DateTime;
use chrono::SecondsFormat;
use chrono::Utc;
use clap::Args;
use clap::Parser;
use clap::Subcommand;
use jackerymon::ble;
use jackerymon::key_file::KeyFile;
use jackerymon::poll::PollConfig;
use jackerymon::poll::PollError;
use jackerymon::poll::PollEvent;
use jackerymon::poll::Sample;
use serde_json::json;
use tokio::signal::ctrl_c;
use tokio::time::Instant;

const MILLIS_PER_SECOND: f64 = 1000.0;
/// `query_ms` is rounded to tenths, as the Python PoC does.
const QUERY_MS_STEPS_PER_MILLI: f64 = 10.0;
const RECONNECT_DELAY: Duration = Duration::from_secs(1);

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

#[derive(Args)]
struct PollArgs {
    /// Key file written by `tools/jackery_key.py`.
    #[arg(long, default_value = "jackery-key.json")]
    key_file: PathBuf,
    /// BLE address, overriding the key file's.
    #[arg(long)]
    address: Option<String>,
    /// Seconds allowed for discovery, connection, and each query.
    #[arg(long, default_value_t = 10.0, value_parser = positive_seconds)]
    timeout: f64,
    /// Seconds between a response and the next request.
    #[arg(long, default_value_t = 5.0, value_parser = positive_seconds)]
    interval: f64,
    /// Stop after this many samples; 0 means until Ctrl-C.
    #[arg(long, default_value_t = 0)]
    count: u64,
    /// Consecutive transport failures that stop polling; 0 means never.
    #[arg(long, default_value_t = 3)]
    max_failures: u32,
}

fn positive_seconds(text: &str) -> Result<f64, String> {
    match text.parse::<f64>() {
        Ok(seconds) if seconds.is_finite() && seconds > 0.0 => Ok(seconds),
        _ => Err("must be a positive number of seconds".to_owned()),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Poll(arguments) => poll(arguments).await,
    }
}

/// Counters for the closing summary.
#[derive(Default)]
struct Summary {
    samples: u64,
    connections: u64,
    transport_failures: u64,
}

async fn poll(arguments: PollArgs) -> Result<()> {
    let mut key_file = KeyFile::load(&arguments.key_file)?;
    if let Some(address) = arguments.address {
        key_file.address = address;
    }
    let config = PollConfig {
        timeout: Duration::from_secs_f64(arguments.timeout),
        interval: Duration::from_secs_f64(arguments.interval),
        reconnect_delay: RECONNECT_DELAY,
        max_consecutive_failures: NonZeroU32::new(arguments.max_failures),
    };
    let limit = config
        .max_consecutive_failures
        .map_or_else(String::new, |limit| format!("/{limit}"));
    let adapter = ble::default_adapter().await?;
    let started = Instant::now();
    let mut summary = Summary::default();
    let stop = async {
        // Without a signal handler Ctrl-C cannot close the connection.
        if ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    let result = ble::poll(adapter, key_file, config, stop, |event| match event {
        PollEvent::Connected { connection } => {
            summary.connections = connection;
            ControlFlow::Continue(())
        }
        PollEvent::Sample(sample) => {
            summary.samples = sample.sample;
            println!("{}", sample_line(&sample));
            if sample.sample == arguments.count {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        }
        PollEvent::TransportFailure { error, consecutive } => {
            summary.transport_failures += 1;
            eprintln!("BLE polling failure {consecutive}{limit}: {error}; reconnecting");
            ControlFlow::Continue(())
        }
    })
    .await;
    if matches!(result, Err(PollError::TooManyFailures { .. })) {
        summary.transport_failures += 1;
    }
    eprintln!(
        "Polling summary: samples={} connections={} transport_failures={} elapsed_s={:.1}",
        summary.samples,
        summary.connections,
        summary.transport_failures,
        started.elapsed().as_secs_f64()
    );
    result.context("polling failed")
}

/// JSON line in the same shape as the Python PoC's `--poll` output.
fn sample_line(sample: &Sample) -> String {
    let query_ms = (sample.query.as_secs_f64() * MILLIS_PER_SECOND * QUERY_MS_STEPS_PER_MILLI)
        .round()
        / QUERY_MS_STEPS_PER_MILLI;
    json!({
        "timestamp": DateTime::<Utc>::from(sample.timestamp)
            .to_rfc3339_opts(SecondsFormat::Micros, false),
        "sample": sample.sample,
        "connection": sample.connection,
        "transport_failures": sample.transport_failures,
        "query_ms": query_ms,
        "status": sample.status,
    })
    .to_string()
}
