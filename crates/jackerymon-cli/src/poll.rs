//! `jackerymon poll`: JSON lines on stdout, diagnostics on stderr.

use std::num::NonZeroU32;
use std::ops::ControlFlow;

use anyhow::Context;
use anyhow::Result;
use chrono::DateTime;
use chrono::SecondsFormat;
use chrono::Utc;
use clap::Args;
use jackerymon::ble;
use jackerymon::poll::PollError;
use jackerymon::poll::PollEvent;
use jackerymon::poll::Sample;
use serde_json::json;
use tokio::signal::ctrl_c;
use tokio::time::Instant;

use crate::connection::ConnectionArgs;

const MILLIS_PER_SECOND: f64 = 1000.0;
/// `query_ms` is rounded to tenths, as the Python PoC does.
const QUERY_MS_STEPS_PER_MILLI: f64 = 10.0;

#[derive(Args)]
pub(crate) struct PollArgs {
    #[command(flatten)]
    connection: ConnectionArgs,
    /// Stop after this many samples; 0 means until Ctrl-C.
    #[arg(long, default_value_t = 0)]
    count: u64,
    /// Consecutive transport failures that stop polling; 0 means never.
    #[arg(long, default_value_t = 3)]
    max_failures: u32,
}

/// Counters for the closing summary.
#[derive(Default)]
struct Summary {
    samples: u64,
    connections: u64,
    transport_failures: u64,
}

pub(crate) async fn run(arguments: PollArgs) -> Result<()> {
    let key_file = arguments.connection.key_file()?;
    let config = arguments
        .connection
        .poll_config(NonZeroU32::new(arguments.max_failures));
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
