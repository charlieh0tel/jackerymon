//! `jackerymon daemon`: own the BLE connection; serve status, metrics,
//! live updates, and settings over HTTP; optionally log every sample.
//!
//! The control endpoint has no authentication, so the default bind is
//! localhost.

mod http;
mod log;
mod metrics;
mod state;

use std::future::Future;
use std::future::IntoFuture;
use std::net::SocketAddr;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
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
use tokio::sync::watch;

use crate::connection::ConnectionArgs;
use crate::connection::KeyHome;
use crate::daemon::log::SampleLog;
use crate::daemon::state::Snapshot;
use crate::record::Update;

const DEFAULT_LISTEN: &str = "127.0.0.1:9888";
/// Updates buffered per `/events` client before it starts missing some.
const EVENT_BUFFER: usize = 64;
/// How often `/events` sends a comment while idle, so a client can tell
/// a quiet daemon from a dead connection.
pub(crate) const EVENT_KEEP_ALIVE: Duration = Duration::from_secs(15);
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
    /// Set once polling has ended, for any reason; HTTP then winds down.
    stopping: watch::Sender<bool>,
}

impl Shared {
    /// The shared state and the receiving end of its commands.
    fn new(serial: String) -> (Arc<Self>, mpsc::Receiver<ControlRequest>) {
        let (updates, _) = broadcast::channel(EVENT_BUFFER);
        let (commands, requests) = mpsc::channel(COMMAND_BUFFER);
        let shared = Self {
            snapshot: Mutex::new(Snapshot::new(serial)),
            updates,
            commands,
            stopping: watch::Sender::new(false),
        };
        (Arc::new(shared), requests)
    }

    /// Completes once polling has ended.
    fn stopped(&self) -> impl Future<Output = ()> + use<> {
        let mut stopping = self.stopping.subscribe();
        async move {
            // An error means the sender is gone, which also means stopped.
            stopping.wait_for(|stopping| *stopping).await.ok();
        }
    }
}

pub(crate) async fn run(arguments: DaemonArgs) -> Result<()> {
    let key_file = arguments.connection.key_file(KeyHome::System)?;
    // A daemon keeps trying while the unit's radio is off.
    let config = arguments.connection.poll_config(None);
    let adapter = ble::default_adapter().await?;
    let listener = TcpListener::bind(arguments.listen)
        .await
        .with_context(|| format!("listening on {}", arguments.listen))?;
    let (shared, requests) = Shared::new(key_file.serial.to_string());
    let mut log = arguments
        .log_dir
        .map(SampleLog::new)
        .transpose()
        .context("sample log")?;
    // Report a failing log once, not every sample, and say when it recovers.
    let mut log_failing = false;
    let poller = {
        let shared = Arc::clone(&shared);
        async move {
            ble::poll(
                adapter,
                key_file,
                config,
                Some(requests),
                shutdown(),
                move |event| {
                    let update = Update::from(event);
                    if let (Some(log), Update::Sample(record)) = (&mut log, &update) {
                        match (log.write(record), log_failing) {
                            (Err(error), false) => {
                                eprintln!(
                                    "sample log: {error:#}; reporting again once it recovers"
                                );
                                log_failing = true;
                            }
                            (Ok(()), true) => {
                                eprintln!("sample log: writing again");
                                log_failing = false;
                            }
                            _ => {}
                        }
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
            .await
            .context("polling failed")
        }
    };
    eprintln!("listening on http://{}", arguments.listen);
    serve(shared, listener, poller).await
}

/// Serves HTTP until `poller` ends, whether a signal stopped it or it
/// failed, then returns its result.  Open `/events` streams are closed
/// so shutdown does not wait on their clients.
async fn serve(
    shared: Arc<Shared>,
    listener: TcpListener,
    poller: impl Future<Output = Result<()>>,
) -> Result<()> {
    let polling = async {
        let polled = poller.await;
        shared.stopping.send_replace(true);
        polled
    };
    let server = axum::serve(listener, http::router(Arc::clone(&shared)))
        .with_graceful_shutdown(shared.stopped())
        .into_future();
    let (polled, served) = tokio::join!(polling, server);
    polled?;
    served.context("serving HTTP")
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use anyhow::anyhow;
    use futures::StreamExt;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio::time::timeout;

    use super::Shared;
    use super::serve;

    /// Long enough for a loaded machine; a hang never ends.
    const PROMPTLY: Duration = Duration::from_secs(10);

    #[tokio::test]
    async fn polling_failure_ends_serving_despite_an_events_client() {
        let (shared, _requests) = Shared::new("123456789".to_owned());
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("http://{}/events", listener.local_addr().expect("address"));
        let (fail, failed) = oneshot::channel::<()>();
        let served = tokio::spawn(serve(shared, listener, async move {
            failed.await.ok();
            Err(anyhow!("bad key"))
        }));
        let response = reqwest::get(&url).await.expect("events");
        let mut events = response.bytes_stream();
        fail.send(()).expect("poller waiting");
        let result = timeout(PROMPTLY, served)
            .await
            .expect("serving ends")
            .expect("task");
        assert_eq!(result.expect_err("poll error").to_string(), "bad key");
        let rest = timeout(PROMPTLY, events.next()).await.expect("stream ends");
        assert!(rest.is_none_or(|chunk| chunk.is_err()));
    }
}
