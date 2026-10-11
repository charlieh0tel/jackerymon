//! `jackerymon daemon`: own the BLE connection; serve status, metrics,
//! live updates, and settings over HTTP; optionally log every sample.
//!
//! The control endpoint has no authentication, so the default bind is
//! localhost.

mod http;
mod log;
mod metrics;
mod state;

use std::net::SocketAddr;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::SystemTime;

use anyhow::Context;
use anyhow::Result;
use clap::Args;
use jackerymon::ble;
use jackerymon::poll::ControlRequest;
use tokio::net::TcpListener;
use tokio::signal::unix::SignalKind;
use tokio::signal::unix::signal;
use tokio::sync::broadcast;
use tokio::sync::mpsc;

use crate::connection::ConnectionArgs;
use crate::daemon::log::SampleLog;
use crate::daemon::state::Snapshot;
use crate::record::Update;

const DEFAULT_LISTEN: &str = "127.0.0.1:9888";
/// Updates buffered per `/events` client before it starts missing some.
const EVENT_BUFFER: usize = 64;
/// Settings queued from HTTP while one is being applied.
const COMMAND_BUFFER: usize = 8;

#[derive(Args)]
pub(crate) struct DaemonArgs {
    #[command(flatten)]
    connection: ConnectionArgs,
    /// Address for the HTTP API.  It can switch outputs and has no
    /// authentication; expose it beyond localhost only on a trusted
    /// network.
    #[arg(long, default_value = DEFAULT_LISTEN)]
    listen: SocketAddr,
    /// Directory for daily `YYYY-MM-DD.jsonl` sample logs; none if unset.
    #[arg(long)]
    log_dir: Option<PathBuf>,
}

/// What the poller and HTTP handlers share.
pub(crate) struct Shared {
    snapshot: Mutex<Snapshot>,
    updates: broadcast::Sender<Update>,
    commands: mpsc::Sender<ControlRequest>,
}

pub(crate) async fn run(arguments: DaemonArgs) -> Result<()> {
    let key_file = arguments.connection.key_file()?;
    // A daemon keeps trying while the unit's radio is off.
    let config = arguments.connection.poll_config(None);
    let adapter = ble::default_adapter().await?;
    let listener = TcpListener::bind(arguments.listen)
        .await
        .with_context(|| format!("listening on {}", arguments.listen))?;
    let (updates, _) = broadcast::channel(EVENT_BUFFER);
    let (commands, requests) = mpsc::channel(COMMAND_BUFFER);
    let shared = Arc::new(Shared {
        snapshot: Mutex::new(Snapshot::new(key_file.serial.to_string())),
        updates,
        commands,
    });
    let mut log = arguments.log_dir.map(SampleLog::new);
    let poller = {
        let shared = Arc::clone(&shared);
        ble::poll(
            adapter,
            key_file,
            config,
            Some(requests),
            shutdown(),
            move |event| {
                let update = Update::from(event);
                if let (Some(log), Update::Sample(record)) = (&mut log, &update)
                    && let Err(error) = log.write(record)
                {
                    eprintln!("sample log: {error:#}");
                }
                shared
                    .snapshot
                    .lock()
                    .expect("snapshot lock")
                    .apply(&update, SystemTime::now());
                // No subscribers is normal.
                shared.updates.send(update).ok();
                ControlFlow::Continue(())
            },
        )
    };
    eprintln!("listening on http://{}", arguments.listen);
    let server = axum::serve(listener, http::router(shared)).with_graceful_shutdown(shutdown());
    let (polled, served) = tokio::join!(poller, server);
    served.context("serving HTTP")?;
    polled.context("polling failed")
}

/// Completes on SIGINT or SIGTERM.
async fn shutdown() {
    let mut terminate = signal(SignalKind::terminate()).expect("installing SIGTERM handler");
    let mut interrupt = signal(SignalKind::interrupt()).expect("installing SIGINT handler");
    tokio::select! {
        _ = terminate.recv() => {}
        _ = interrupt.recv() => {}
    }
}
