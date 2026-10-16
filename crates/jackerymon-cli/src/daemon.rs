//! `jackerymon daemon`: own the BLE connections to one or more units;
//! serve status, metrics, live updates, and settings over HTTP;
//! optionally log every sample.
//!
//! Each unit's full API is on its own Unix socket, whose permissions
//! decide who may use it.  TCP, for Prometheus and the browser, serves
//! every unit under `/units/<serial>/`, read-only unless
//! `--tcp-control`.

mod http;
pub(crate) mod log;
mod metrics;
mod state;
mod web;

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
use anyhow::bail;
use clap::Args;
use futures::future::join_all;
use jackerymon::ble;
use jackerymon::identity::ModelCode;
use jackerymon::key_file::KeyFile;
use jackerymon::poll::ControlRequest;
use jackerymon::poll::PollConfig;
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
/// How long open requests may finish after the daemon starts stopping.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// Settings queued from HTTP while one is being applied.
const COMMAND_BUFFER: usize = 8;
/// How long a unit waits to poll again after polling failed, as systemd
/// waited to restart a daemon that served one unit.
const RESTART_DELAY: Duration = Duration::from_secs(5);

/// Options of `jackerymon daemon`.
#[derive(Args)]
pub(crate) struct DaemonArgs {
    /// Without `--unit` or `--key-file`, every unit with a key in the key
    /// directory.
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
    /// Unix socket for the full API of the one unit served; default
    /// `$XDG_RUNTIME_DIR/jackerymon/<serial>.sock`, or systemd's runtime
    /// directory.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Directory for daily `<serial>/YYYY-MM-DD.jsonl` sample logs; none
    /// if unset.
    #[arg(long)]
    log_dir: Option<PathBuf>,
}

/// One unit: what its poller and HTTP handlers share.
struct Unit {
    serial: String,
    model: ModelCode,
    snapshot: Mutex<Snapshot>,
    updates: broadcast::Sender<Update>,
    /// The running poll's settings queue.  Replaced each time polling
    /// starts, since a poll that ends takes its receiver with it.
    commands: Mutex<mpsc::Sender<ControlRequest>>,
    /// Turns true when the daemon stops; HTTP then winds down.
    stopping: watch::Receiver<bool>,
    /// This unit's directory under `--log-dir`, which `/history` reads;
    /// `None` without one.
    log_dir: Option<PathBuf>,
}

impl Unit {
    fn new(
        serial: String,
        model: ModelCode,
        log_dir: Option<PathBuf>,
        stopping: watch::Receiver<bool>,
    ) -> Arc<Self> {
        let (updates, _) = broadcast::channel(EVENT_BUFFER);
        // Closed until polling starts, so a setting fails at once.
        let (commands, _) = mpsc::channel(COMMAND_BUFFER);
        Arc::new(Self {
            snapshot: Mutex::new(Snapshot::new(serial.clone())),
            serial,
            model,
            updates,
            commands: Mutex::new(commands),
            stopping,
            log_dir,
        })
    }

    /// A new settings queue, for a poll about to start.
    fn take_commands(&self) -> mpsc::Receiver<ControlRequest> {
        let (commands, requests) = mpsc::channel(COMMAND_BUFFER);
        *self.commands.lock().expect("commands lock") = commands;
        requests
    }

    /// Completes once the daemon is stopping.
    fn stopped(&self) -> impl Future<Output = ()> + use<> {
        stopped(self.stopping.clone())
    }
}

/// Completes once `stopping` turns true, or its sender is gone, which
/// also means stopped.
async fn stopped(mut stopping: watch::Receiver<bool>) {
    stopping.wait_for(|stopping| *stopping).await.ok();
}

pub(crate) async fn run(arguments: DaemonArgs) -> Result<()> {
    let key_files = arguments.connection.key_files(KeyHome::System)?;
    if arguments.socket.is_some() && key_files.len() > 1 {
        bail!("--socket is for one unit; choose it with --unit");
    }
    // A daemon keeps trying while a unit's radio is off.
    let config = arguments.connection.poll_config(None);
    let tcp = TcpListener::bind(arguments.listen)
        .await
        .with_context(|| format!("listening on {}", arguments.listen))?;
    let (stop, stopping) = watch::channel(false);
    let mut units = Vec::new();
    let mut sockets = Vec::new();
    // Held until the daemon exits, which removes the sockets.
    let mut claims = Vec::new();
    let mut pollers = Vec::new();
    for key_file in key_files {
        let serial = key_file.serial.to_string();
        let socket_path = match &arguments.socket {
            Some(path) => path.clone(),
            None => runtime::daemon_path(&serial)?,
        };
        let (socket, claimed) = runtime::claim(&socket_path)?;
        let log_dir = arguments
            .log_dir
            .as_ref()
            .map(|directory| directory.join(&serial));
        let log = log_dir
            .clone()
            .map(SampleLog::new)
            .transpose()
            .with_context(|| format!("{serial}: sample log"))?;
        let unit = Unit::new(serial.clone(), key_file.model, log_dir, stopping.clone());
        info!("{serial}: listening on {}", socket_path.display());
        pollers.push(keep_polling(Arc::clone(&unit), key_file, config, log));
        sockets.push((Arc::clone(&unit), socket));
        claims.push(claimed);
        units.push(unit);
    }
    info!(
        "listening on http://{} ({})",
        arguments.listen,
        if arguments.tcp_control {
            "read-write"
        } else {
            "read-only"
        }
    );
    let signalled = stop.clone();
    let polling = async move {
        tokio::join!(join_all(pollers), async {
            shutdown().await;
            signalled.send_replace(true);
        });
        Ok(())
    };
    let listeners = Listeners {
        sockets,
        tcp,
        tcp_control: arguments.tcp_control,
    };
    serve(units, listeners, polling, stop).await
}

/// Polls `unit` until the daemon stops, starting again after a failure:
/// the unit held by another process, no adapter, or polling ended by an
/// error.
async fn keep_polling(
    unit: Arc<Unit>,
    key_file: KeyFile,
    config: PollConfig,
    mut log: Option<SampleLog>,
) {
    // Report a failing log once, not every sample, and say when it
    // recovers.
    let mut log_failing = false;
    // The last failure reported, so one that repeats, such as another
    // process holding the unit, is reported once until polling gets a
    // sample or fails another way.
    let mut reported: Option<String> = None;
    loop {
        let samples = unit.snapshot.lock().expect("snapshot lock").samples;
        let polled = poll(&unit, &key_file, config, &mut log, &mut log_failing).await;
        let Err(failure) = polled else {
            return;
        };
        let failure = format!("{failure:#}");
        {
            let mut snapshot = unit.snapshot.lock().expect("snapshot lock");
            if snapshot.samples != samples || reported.as_ref() != Some(&failure) {
                error!(
                    "{}: {failure}; trying every {RESTART_DELAY:?}, reporting again once it changes",
                    unit.serial
                );
                reported = Some(failure.clone());
            }
            snapshot.fail(failure);
        }
        tokio::select! {
            () = sleep(RESTART_DELAY) => {}
            () = unit.stopped() => return,
        }
    }
}

/// Polls `unit` once, until the daemon stops or polling fails.
async fn poll(
    unit: &Unit,
    key_file: &KeyFile,
    config: PollConfig,
    log: &mut Option<SampleLog>,
    log_failing: &mut bool,
) -> Result<()> {
    let _lock = runtime::lock_unit(&unit.serial)?;
    // A BlueZ session of its own: BlueZ keeps one scan filter per D-Bus
    // client, so units sharing a session would overwrite each other's.
    let adapter = ble::default_adapter().await?;
    let serial = &unit.serial;
    ble::poll(
        adapter,
        key_file.clone(),
        config,
        Some(unit.take_commands()),
        unit.stopped(),
        |event| {
            let update = Update::from(event);
            match &update {
                Update::Connected { connection } => {
                    info!("{serial}: connected (connection {connection})");
                }
                Update::Lost { error, consecutive } => {
                    warn!("{serial}: BLE failure {consecutive}: {error}; reconnecting");
                }
                Update::Sample(_) => {}
            }
            if let (Some(log), Update::Sample(record)) = (log.as_mut(), &update) {
                match (log.write(record), *log_failing) {
                    (Err(error), false) => {
                        error!("{serial}: sample log: {error:#}; reporting again once it recovers");
                        *log_failing = true;
                    }
                    (Ok(()), true) => {
                        info!("{serial}: sample log: writing again");
                        *log_failing = false;
                    }
                    _ => {}
                }
            }
            unit.snapshot
                .lock()
                .expect("snapshot lock")
                .apply(&update, SystemTime::now());
            // No subscribers is normal.
            unit.updates.send(update).ok();
            ControlFlow::Continue(())
        },
    )
    .await
    .context("polling failed")
}

/// Where the daemon serves.
struct Listeners {
    /// Each unit's full API.
    sockets: Vec<(Arc<Unit>, UnixListener)>,
    /// Every unit, under `/units/<serial>/`.
    tcp: TcpListener,
    /// Whether TCP also takes settings.
    tcp_control: bool,
}

/// Serves HTTP until `poller` ends, then sets `stop` and returns its
/// result.  Open `/events` streams are closed so shutdown does not wait
/// on their clients.
async fn serve(
    units: Vec<Arc<Unit>>,
    listeners: Listeners,
    poller: impl Future<Output = Result<()>>,
    stop: watch::Sender<bool>,
) -> Result<()> {
    let polling = async {
        let polled = poller.await;
        stop.send_replace(true);
        polled
    };
    let sockets = join_all(listeners.sockets.into_iter().map(|(unit, socket)| {
        axum::serve(socket, http::socket_router(unit))
            .with_graceful_shutdown(stopped(stop.subscribe()))
            .into_future()
    }));
    let tcp = axum::serve(
        listeners.tcp,
        http::tcp_router(&units, listeners.tcp_control),
    )
    .with_graceful_shutdown(stopped(stop.subscribe()))
    .into_future();
    // Requests still open this long after stopping starts are dropped, so
    // a client that never finishes one cannot hold the daemon.
    let servers = async { tokio::join!(sockets, tcp) };
    let bounded = async {
        tokio::select! {
            served = servers => served,
            () = async {
                stopped(stop.subscribe()).await;
                sleep(SHUTDOWN_GRACE).await;
            } => {
                warn!("dropping requests still open {SHUTDOWN_GRACE:?} after stopping");
                (Vec::new(), Ok(()))
            }
        }
    };
    let (polled, (sockets, tcp)) = tokio::join!(polling, bounded);
    polled?;
    for socket in sockets {
        socket.context("serving a socket")?;
    }
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
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::anyhow;
    use futures::StreamExt;
    use reqwest::StatusCode;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;
    use tokio::net::UnixStream;
    use tokio::sync::oneshot;
    use tokio::sync::watch;
    use tokio::time::timeout;

    use super::Listeners;
    use super::SHUTDOWN_GRACE;
    use super::Unit;
    use super::serve;
    use crate::record::Update;
    use jackerymon::control::Setting;
    use jackerymon::identity::ModelCode;
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
        let (halt, halting) = watch::channel(false);
        let unit = Unit::new(
            "123456789".to_owned(),
            ModelCode::EXPLORER_2000,
            None,
            halting,
        );
        let path = socket_path("failure");
        let (socket, claimed) = runtime::claim(&path).expect("socket");
        let tcp = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let base = format!(
            "http://{}/units/123456789",
            tcp.local_addr().expect("address")
        );
        let (fail, failed) = oneshot::channel::<()>();
        let listeners = Listeners {
            sockets: vec![(Arc::clone(&unit), socket)],
            tcp,
            tcp_control: false,
        };
        let served = tokio::spawn(serve(
            vec![unit],
            listeners,
            async move {
                failed.await.ok();
                Err(anyhow!("bad key"))
            },
            halt,
        ));
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
        let (halt, halting) = watch::channel(false);
        let unit = Unit::new(
            "123456789".to_owned(),
            ModelCode::EXPLORER_2000,
            None,
            halting,
        );
        let path = socket_path("stalled");
        let (socket, claimed) = runtime::claim(&path).expect("socket");
        let tcp = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let (fail, failed) = oneshot::channel::<()>();
        let listeners = Listeners {
            sockets: vec![(Arc::clone(&unit), socket)],
            tcp,
            tcp_control: false,
        };
        let served = tokio::spawn(serve(
            vec![unit],
            listeners,
            async move {
                failed.await.ok();
                Err(anyhow!("stopped"))
            },
            halt,
        ));
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
            let (halt, halting) = watch::channel(false);
            let unit = Unit::new(serial.to_owned(), ModelCode::EXPLORER_2000, None, halting);
            let (socket, claimed) = runtime::claim(path).expect("socket");
            let tcp = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let (stop, stopped) = oneshot::channel::<()>();
            let listeners = Listeners {
                sockets: vec![(Arc::clone(&unit), socket)],
                tcp,
                tcp_control: false,
            };
            let served = tokio::spawn(serve(
                vec![unit],
                listeners,
                async move {
                    stopped.await.ok();
                    Ok(())
                },
                halt,
            ));
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
    async fn tcp_serves_every_unit() {
        let (halt, halting) = watch::channel(false);
        let serials = ["111", "222"];
        let directory = socket_path("units").parent().expect("directory").to_owned();
        let mut units = Vec::new();
        let mut sockets = Vec::new();
        let mut claims = Vec::new();
        for serial in serials {
            let unit = Unit::new(
                serial.to_owned(),
                ModelCode::EXPLORER_2000,
                None,
                halting.clone(),
            );
            let (socket, claimed) =
                runtime::claim(&directory.join(format!("{serial}.sock"))).expect("socket");
            sockets.push((Arc::clone(&unit), socket));
            claims.push(claimed);
            units.push(unit);
        }
        let connected = Arc::clone(&units[1]);
        let tcp = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let base = format!("http://{}", tcp.local_addr().expect("address"));
        let listeners = Listeners {
            sockets,
            tcp,
            tcp_control: false,
        };
        let (stop, stopped) = oneshot::channel::<()>();
        let served = tokio::spawn(serve(
            units,
            listeners,
            async move {
                stopped.await.ok();
                Ok(())
            },
            halt,
        ));
        let get = |path: &str| reqwest::get(format!("{base}{path}"));
        // A browser at the root lands on the first unit's page, whose
        // address ends in `/` so the page's relative reads resolve.
        for path in ["/", "/units/222"] {
            let page = get(path).await.expect("page");
            assert!(page.url().path().ends_with('/'), "{}", page.url());
            assert!(
                page.text()
                    .await
                    .expect("body")
                    .starts_with("<!doctype html>")
            );
        }
        let landed = get("/?last=600").await.expect("page");
        assert_eq!(landed.url().path(), "/units/111/");
        assert_eq!(landed.url().query(), Some("last=600"));
        let listed: serde_json::Value = get("/units")
            .await
            .expect("units")
            .json()
            .await
            .expect("json");
        assert_eq!(listed[1]["serial"], "222");
        assert_eq!(listed[1]["link"]["state"], "searching");
        let status: serde_json::Value = get("/units/222/status")
            .await
            .expect("status")
            .json()
            .await
            .expect("json");
        assert_eq!(status["serial"], "222");
        assert_eq!(status["model_name"], "Explorer 2000 v2");
        // By serial, connected or not.
        connected.snapshot.lock().expect("snapshot lock").apply(
            &Update::Connected { connection: 1 },
            std::time::SystemTime::now(),
        );
        assert_eq!(get("/").await.expect("page").url().path(), "/units/111/");
        let listed: serde_json::Value = get("/units")
            .await
            .expect("units")
            .json()
            .await
            .expect("json");
        assert_eq!(listed[0]["serial"], "111");
        assert_eq!(listed[1]["link"]["state"], "connected");
        let metrics = get("/metrics")
            .await
            .expect("metrics")
            .text()
            .await
            .expect("text");
        for serial in serials {
            assert!(
                metrics.contains(&format!("jackery_connected{{serial=\"{serial}\"}}")),
                "{metrics}"
            );
        }
        stop.send(()).expect("stop");
        served.await.expect("task").expect("served");
        drop(claims);
        fs::remove_dir_all(&directory).expect("cleanup");
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
            let (halt, halting) = watch::channel(false);
            let unit = Unit::new(
                "123456789".to_owned(),
                ModelCode::EXPLORER_2000,
                log_dir,
                halting,
            );
            let path = directory.join("123456789.sock");
            let (socket, claimed) = runtime::claim(&path).expect("socket");
            let tcp = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let base = format!(
                "http://{}/units/123456789",
                tcp.local_addr().expect("address")
            );
            let (stop, stopped) = oneshot::channel::<()>();
            let listeners = Listeners {
                sockets: vec![(Arc::clone(&unit), socket)],
                tcp,
                tcp_control: false,
            };
            let served = tokio::spawn(serve(
                vec![unit],
                listeners,
                async move {
                    stopped.await.ok();
                    Ok(())
                },
                halt,
            ));
            let response = reqwest::get(format!("{base}/history?{query}"))
                .await
                .expect("history");
            assert_eq!(response.status(), expected, "{query}");
            if expected == StatusCode::OK {
                assert_eq!(response.text().await.expect("body"), format!("{line}\n"));
                // The page and its chart data come from the same server.
                for (path, kind) in [
                    ("/", "text/html"),
                    ("/style.css", "text/css"),
                    ("/common.js", "text/javascript"),
                ] {
                    let page = reqwest::get(format!("{base}{path}")).await.expect("page");
                    let content_type = page.headers()[reqwest::header::CONTENT_TYPE]
                        .to_str()
                        .expect("type")
                        .to_owned();
                    assert!(content_type.starts_with(kind), "{path}: {content_type}");
                    assert_eq!(page.headers()[reqwest::header::CACHE_CONTROL], "no-cache");
                }
                // A fixed end, or the range grows with the clock until
                // `every` widens to keep under the point cap.
                let series: serde_json::Value = reqwest::get(format!(
                    "{base}/series?{query}&until=2026-10-12T00:00:00Z&every=60"
                ))
                .await
                .expect("series")
                .json()
                .await
                .expect("json");
                // The one sample's minute, among gaps where the range runs
                // past it.
                let taken = chrono::DateTime::parse_from_rfc3339("2026-10-11T12:00:00Z")
                    .expect("time")
                    .timestamp();
                let times = series["time"].as_array().expect("column");
                assert!(times.contains(&taken.into()), "{times:?}");
                assert_eq!(series["every"], 60);
            }
            stop.send(()).expect("stop");
            served.await.expect("task").expect("served");
            drop(claimed);
        }
        fs::remove_dir_all(&directory).expect("cleanup");
    }
}
