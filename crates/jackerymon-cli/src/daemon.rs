//! `jackerymon daemon`: own the BLE connection; serve status, metrics,
//! live updates, and settings over HTTP; optionally log every sample.
//!
//! The full API is on a Unix socket, whose permissions decide who may
//! use it.  TCP, for Prometheus, is read-only unless `--tcp-control`.

mod http;
pub(crate) mod log;
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

use ::log::error;
use ::log::info;
use ::log::warn;
use anyhow::Context;
use anyhow::Result;
use clap::Args;
use jackerymon::ble;
use jackerymon::poll::ControlRequest;
use tokio::net::TcpListener;
use tokio::net::UnixListener;
use tokio::signal::unix::SignalKind;
use tokio::signal::unix::signal;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio::time::sleep;

use crate::connection::ConnectionArgs;
use crate::connection::KeyHome;
use crate::daemon::log::SampleLog;
use crate::daemon::state::Snapshot;
use crate::record::Update;
use crate::runtime;

const DEFAULT_LISTEN: &str = "127.0.0.1:9888";
/// Updates buffered per `/events` client before it starts missing some.
const EVENT_BUFFER: usize = 64;
/// How often `/events` sends a comment while idle, so a client can tell
/// a quiet daemon from a dead connection.
pub(crate) const EVENT_KEEP_ALIVE: Duration = Duration::from_secs(15);
/// How long open requests may finish after polling ends.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// Settings queued from HTTP while one is being applied.
const COMMAND_BUFFER: usize = 8;

/// Options of `jackerymon daemon`.
#[derive(Args)]
pub(crate) struct DaemonArgs {
    #[command(flatten)]
    connection: ConnectionArgs,
    /// TCP address for the read-only routes: metrics, status, events.
    #[arg(long, env = "JACKERYMON_LISTEN", default_value = DEFAULT_LISTEN)]
    listen: SocketAddr,
    /// Also accept settings over TCP.  TCP has no authentication; anyone
    /// who can reach it, including web pages through DNS rebinding, can
    /// switch outputs.
    #[arg(long, env = "JACKERYMON_TCP_CONTROL")]
    tcp_control: bool,
    /// Unix socket for the full API; default
    /// `$XDG_RUNTIME_DIR/jackerymon/<serial>.sock`, or systemd's runtime
    /// directory.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Directory for daily `YYYY-MM-DD.jsonl` sample logs; none if unset.
    #[arg(long)]
    log_dir: Option<PathBuf>,
}

/// What the poller and HTTP handlers share.
struct Shared {
    snapshot: Mutex<Snapshot>,
    updates: broadcast::Sender<Update>,
    commands: mpsc::Sender<ControlRequest>,
    /// Set once polling has ended, for any reason; HTTP then winds down.
    stopping: watch::Sender<bool>,
    /// `--log-dir`, which `/history` reads; `None` without one.
    log_dir: Option<PathBuf>,
}

impl Shared {
    /// The shared state and the receiving end of its commands.
    fn new(
        serial: String,
        log_dir: Option<PathBuf>,
    ) -> (Arc<Self>, mpsc::Receiver<ControlRequest>) {
        let (updates, _) = broadcast::channel(EVENT_BUFFER);
        let (commands, requests) = mpsc::channel(COMMAND_BUFFER);
        let shared = Self {
            snapshot: Mutex::new(Snapshot::new(serial)),
            updates,
            commands,
            stopping: watch::Sender::new(false),
            log_dir,
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
    let (key_file, _lock) = arguments.connection.claim(KeyHome::System)?;
    // A daemon keeps trying while the unit's radio is off.
    let config = arguments.connection.poll_config(None);
    let adapter = ble::default_adapter().await?;
    let socket_path = match arguments.socket {
        Some(path) => path,
        None => runtime::daemon_path(key_file.serial.as_str())?,
    };
    let (socket, _claimed) = runtime::claim(&socket_path)?;
    let listener = TcpListener::bind(arguments.listen)
        .await
        .with_context(|| format!("listening on {}", arguments.listen))?;
    let (shared, requests) = Shared::new(key_file.serial.to_string(), arguments.log_dir.clone());
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
                    match &update {
                        Update::Connected { connection } => {
                            info!("connected (connection {connection})")
                        }
                        Update::Lost { error, consecutive } => {
                            warn!("BLE failure {consecutive}: {error}; reconnecting");
                        }
                        Update::Sample(_) => {}
                    }
                    if let (Some(log), Update::Sample(record)) = (&mut log, &update) {
                        match (log.write(record), log_failing) {
                            (Err(error), false) => {
                                error!("sample log: {error:#}; reporting again once it recovers");
                                log_failing = true;
                            }
                            (Ok(()), true) => {
                                info!("sample log: writing again");
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
    info!(
        "listening on {} and http://{} ({})",
        socket_path.display(),
        arguments.listen,
        if arguments.tcp_control {
            "read-write"
        } else {
            "read-only"
        }
    );
    let listeners = Listeners {
        socket,
        tcp: listener,
        tcp_control: arguments.tcp_control,
    };
    serve(shared, listeners, poller).await
}

/// Serves HTTP until `poller` ends, whether a signal stopped it or it
/// failed, then returns its result.  Open `/events` streams are closed
/// so shutdown does not wait on their clients.
/// Where the daemon serves.
struct Listeners {
    /// The full API.
    socket: UnixListener,
    tcp: TcpListener,
    /// Whether TCP also takes settings.
    tcp_control: bool,
}

async fn serve(
    shared: Arc<Shared>,
    listeners: Listeners,
    poller: impl Future<Output = Result<()>>,
) -> Result<()> {
    let polling = async {
        let polled = poller.await;
        shared.stopping.send_replace(true);
        polled
    };
    let socket = axum::serve(listeners.socket, http::router(Arc::clone(&shared), true))
        .with_graceful_shutdown(shared.stopped())
        .into_future();
    let tcp = axum::serve(
        listeners.tcp,
        http::router(Arc::clone(&shared), listeners.tcp_control),
    )
    .with_graceful_shutdown(shared.stopped())
    .into_future();
    // Requests still open this long after polling ends are dropped, so a
    // client that never finishes one cannot hold the daemon.
    let servers = async { tokio::join!(socket, tcp) };
    let bounded = async {
        tokio::select! {
            served = servers => served,
            () = async {
                shared.stopped().await;
                sleep(SHUTDOWN_GRACE).await;
            } => {
                warn!("dropping requests still open {SHUTDOWN_GRACE:?} after polling ended");
                (Ok(()), Ok(()))
            }
        }
    };
    let (polled, (socket, tcp)) = tokio::join!(polling, bounded);
    polled?;
    socket.context("serving the socket")?;
    tcp.context("serving TCP")
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
    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;

    use anyhow::anyhow;
    use futures::StreamExt;
    use reqwest::StatusCode;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;
    use tokio::net::UnixStream;
    use tokio::sync::oneshot;
    use tokio::time::timeout;

    use super::Listeners;
    use super::SHUTDOWN_GRACE;
    use super::Shared;
    use super::serve;
    use jackerymon::control::Setting;
    use jackerymon::status::LightMode;

    use crate::daemon_client::DaemonClient;
    use crate::daemon_client::Endpoint;
    use crate::daemon_client::SOCKET_BASE;
    use crate::runtime;

    /// Long enough for a loaded machine; a hang never ends.
    const PROMPTLY: Duration = Duration::from_secs(10);

    /// A socket path of its own for each test.
    fn socket_path(test: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("jackerymon-daemon-{}-{test}", std::process::id()))
            .join("123456789.sock")
    }

    #[tokio::test]
    async fn polling_failure_ends_serving_despite_events_clients() {
        let (shared, _requests) = Shared::new("123456789".to_owned(), None);
        let path = socket_path("failure");
        let (socket, claimed) = runtime::claim(&path).expect("socket");
        let tcp = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let base = format!("http://{}", tcp.local_addr().expect("address"));
        let (fail, failed) = oneshot::channel::<()>();
        let listeners = Listeners {
            socket,
            tcp,
            tcp_control: false,
        };
        let served = tokio::spawn(serve(shared, listeners, async move {
            failed.await.ok();
            Err(anyhow!("bad key"))
        }));
        let over_socket = reqwest::Client::builder()
            .unix_socket(path.clone())
            .build()
            .expect("client");
        let mut tcp_events = reqwest::get(format!("{base}/events"))
            .await
            .expect("events")
            .bytes_stream();
        let mut socket_events = over_socket
            .get(format!("{SOCKET_BASE}/events"))
            .send()
            .await
            .expect("events")
            .bytes_stream();
        let tcp_setting = reqwest::Client::new()
            .post(format!("{base}/settings"))
            .json(&serde_json::json!({"lm": 1}))
            .send()
            .await
            .expect("post");
        assert_eq!(
            tcp_setting.status(),
            StatusCode::NOT_FOUND,
            "TCP is read-only"
        );
        let endpoint = Endpoint::Unix(path.clone());
        assert!(
            DaemonClient::connect(&endpoint, Some("123456789"))
                .await
                .is_ok()
        );
        let other = DaemonClient::connect(&endpoint, Some("987654321"))
            .await
            .err()
            .expect("another unit's daemon is refused")
            .to_string();
        assert!(
            other.contains("owns unit 123456789, not 987654321"),
            "{other}"
        );
        fail.send(()).expect("poller waiting");
        let result = timeout(PROMPTLY, served)
            .await
            .expect("serving ends")
            .expect("task");
        assert_eq!(result.expect_err("poll error").to_string(), "bad key");
        for events in [&mut tcp_events, &mut socket_events] {
            let rest = timeout(PROMPTLY, events.next()).await.expect("stream ends");
            assert!(rest.is_none_or(|chunk| chunk.is_err()));
        }
        drop(claimed);
        fs::remove_dir_all(path.parent().expect("directory")).expect("cleanup");
    }

    #[tokio::test]
    async fn a_request_never_finished_does_not_hold_shutdown() {
        let (shared, _requests) = Shared::new("123456789".to_owned(), None);
        let path = socket_path("stalled");
        let (socket, claimed) = runtime::claim(&path).expect("socket");
        let tcp = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let (fail, failed) = oneshot::channel::<()>();
        let listeners = Listeners {
            socket,
            tcp,
            tcp_control: false,
        };
        let served = tokio::spawn(serve(shared, listeners, async move {
            failed.await.ok();
            Err(anyhow!("stopped"))
        }));
        // Headers promise a body that never comes.
        let mut stalled = UnixStream::connect(&path).await.expect("connect");
        stalled
            .write_all(
                b"POST /settings HTTP/1.1\r\nHost: localhost\r\n\
                  Content-Type: application/json\r\nContent-Length: 100\r\n\r\n{",
            )
            .await
            .expect("write");
        fail.send(()).expect("poller waiting");
        let result = timeout(SHUTDOWN_GRACE + PROMPTLY, served)
            .await
            .expect("serving ends despite the stalled request")
            .expect("task");
        assert!(result.is_err());
        drop(claimed);
        fs::remove_dir_all(path.parent().expect("directory")).expect("cleanup");
    }

    #[tokio::test]
    async fn a_client_notices_another_unit_at_its_address() {
        /// Serves `serial` on `path` until `stop` fires.
        async fn daemon_for(
            serial: &str,
            path: &std::path::Path,
        ) -> (
            oneshot::Sender<()>,
            tokio::task::JoinHandle<anyhow::Result<()>>,
            runtime::Claimed,
        ) {
            let (shared, _requests) = Shared::new(serial.to_owned(), None);
            let (socket, claimed) = runtime::claim(path).expect("socket");
            let tcp = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let (stop, stopped) = oneshot::channel::<()>();
            let listeners = Listeners {
                socket,
                tcp,
                tcp_control: false,
            };
            let served = tokio::spawn(serve(shared, listeners, async move {
                stopped.await.ok();
                Ok(())
            }));
            (stop, served, claimed)
        }
        let path = socket_path("swap");
        let endpoint = Endpoint::Unix(path.clone());
        let (stop, served, claimed) = daemon_for("111", &path).await;
        let (client, _) = DaemonClient::connect(&endpoint, None)
            .await
            .expect("connect");
        stop.send(()).expect("stop");
        served.await.expect("task").expect("served");
        drop(claimed);
        let (stop, served, claimed) = daemon_for("222", &path).await;
        let refused = client
            .post_setting(Setting::Light(LightMode::Low), false)
            .await
            .expect_err("another unit's daemon");
        assert!(refused.contains("now owns unit 222, not 111"), "{refused}");
        stop.send(()).expect("stop");
        served.await.expect("task").expect("served");
        drop(claimed);
        fs::remove_dir_all(path.parent().expect("directory")).expect("cleanup");
    }

    #[tokio::test]
    async fn history_streams_logged_samples_over_tcp() {
        let directory = socket_path("history")
            .parent()
            .expect("directory")
            .to_owned();
        fs::create_dir_all(&directory).expect("directory");
        let line = "{\"timestamp\":\"2026-10-11T12:00:00.000000+00:00\",\"sample\":7,\"connection\":1,\"transport_failures\":0,\"query_ms\":1.0,\"status\":{}}";
        fs::write(directory.join("2026-10-11.jsonl"), format!("{line}\n")).expect("log");
        for (log_dir, query, expected) in [
            (
                Some(directory.clone()),
                "since=2026-10-11T00:00:00Z",
                StatusCode::OK,
            ),
            (None, "since=2026-10-11T00:00:00Z", StatusCode::NOT_FOUND),
            (
                Some(directory.clone()),
                "since=yesterday",
                StatusCode::BAD_REQUEST,
            ),
        ] {
            let (shared, _requests) = Shared::new("123456789".to_owned(), log_dir);
            let path = directory.join("123456789.sock");
            let (socket, claimed) = runtime::claim(&path).expect("socket");
            let tcp = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let base = format!("http://{}", tcp.local_addr().expect("address"));
            let (stop, stopped) = oneshot::channel::<()>();
            let listeners = Listeners {
                socket,
                tcp,
                tcp_control: false,
            };
            let served = tokio::spawn(serve(shared, listeners, async move {
                stopped.await.ok();
                Ok(())
            }));
            let response = reqwest::get(format!("{base}/history?{query}"))
                .await
                .expect("history");
            assert_eq!(response.status(), expected, "{query}");
            if expected == StatusCode::OK {
                assert_eq!(response.text().await.expect("body"), format!("{line}\n"));
            }
            stop.send(()).expect("stop");
            served.await.expect("task").expect("served");
            drop(claimed);
        }
        fs::remove_dir_all(&directory).expect("cleanup");
    }
}
