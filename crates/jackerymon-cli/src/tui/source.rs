//! Where the screen's updates come from and where its settings go.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use jackerymon::ble;
use jackerymon::control::Setting;
use jackerymon::poll::ControlRequest;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::sleep;

use crate::connection::ConnectionArgs;
use crate::connection::KeyHome;
use crate::daemon_client::DaemonClient;
use crate::daemon_client::Endpoint;
use crate::record::Update;

/// Delay before reconnecting to a daemon's event stream.
const DAEMON_RETRY_DELAY: Duration = Duration::from_secs(2);

/// A setting the screen asks for; a failure is answered with a message.
pub(super) struct Command {
    pub(super) setting: Setting,
    pub(super) reply: oneshot::Sender<Result<(), String>>,
}

/// A running source of updates and taker of settings.
pub(super) struct Source {
    pub(super) serial: String,
    pub(super) updates: mpsc::UnboundedReceiver<Update>,
    /// One setting at a time; the screen refuses another while one runs.
    pub(super) commands: mpsc::Sender<Command>,
    stop: oneshot::Sender<()>,
    task: JoinHandle<Result<()>>,
}

impl Source {
    /// Stops the source and reports how it ended.
    pub(super) async fn finish(self) -> Result<()> {
        drop(self.commands);
        // The source may already have ended, closing the receiver.
        self.stop.send(()).ok();
        self.task.await.context("source task failed")?
    }
}

/// Polls the unit over BLE in this process.
pub(super) async fn direct(connection: &ConnectionArgs) -> Result<Source> {
    let (key_file, lock) = connection.claim(KeyHome::User)?;
    let serial = key_file.serial.to_string();
    // A monitor keeps trying while the unit's radio is off.
    let config = connection.poll_config(None);
    let adapter = ble::default_adapter().await?;
    let (sender, updates) = mpsc::unbounded_channel();
    let (commands, mut asked) = mpsc::channel::<Command>(1);
    let (requests, received) = mpsc::channel(1);
    let (stop, stopped) = oneshot::channel::<()>();
    let poll = ble::poll(
        adapter,
        key_file,
        config,
        Some(received),
        async {
            // A dropped sender also means stop.
            stopped.await.ok();
        },
        move |event| {
            if sender.send(Update::from(event)).is_ok() {
                ControlFlow::Continue(())
            } else {
                ControlFlow::Break(())
            }
        },
    );
    let forward = async move {
        while let Some(command) = asked.recv().await {
            let (reply, answer) = oneshot::channel();
            let request = ControlRequest {
                setting: command.setting,
                reply,
            };
            let result = match requests.send(request).await {
                Ok(()) => match answer.await {
                    Ok(result) => result.map_err(|error| error.to_string()),
                    Err(_) => Err("polling stopped".to_owned()),
                },
                Err(_) => Err("polling stopped".to_owned()),
            };
            command.reply.send(result).ok();
        }
    };
    let task = tokio::spawn(async move {
        // Held for as long as the source polls.
        let _lock = lock;
        let (polled, ()) = tokio::join!(poll, forward);
        polled.context("polling failed")
    });
    Ok(Source {
        serial,
        updates,
        commands,
        stop,
        task,
    })
}

/// Follows a running `jackerymon daemon`.  The daemon owns the unit; this
/// only reads its events and posts settings to it.
/// Fails if the daemon owns a unit other than `expected`.
pub(super) async fn daemon(endpoint: &Endpoint, expected: Option<&str>) -> Result<Source> {
    let (client, serial) = DaemonClient::connect(endpoint, expected).await?;
    let (sender, updates) = mpsc::unbounded_channel();
    let (commands, mut asked) = mpsc::channel::<Command>(1);
    let (stop, stopped) = oneshot::channel::<()>();
    let client = Arc::new(client);
    let follow = {
        let client = Arc::clone(&client);
        async move {
            let mut consecutive = 0;
            loop {
                let error = client.follow(&sender, &mut consecutive).await;
                consecutive += 1;
                let lost = Update::Lost {
                    error: format!("daemon: {error:#}"),
                    consecutive,
                };
                if sender.send(lost).is_err() {
                    return;
                }
                sleep(DAEMON_RETRY_DELAY).await;
            }
        }
    };
    let forward = async move {
        while let Some(command) = asked.recv().await {
            // The screen confirmed any warning before sending.
            let result = client.post_setting(command.setting, true).await;
            command.reply.send(result).ok();
        }
    };
    let task = tokio::spawn(async move {
        tokio::select! {
            () = follow => {}
            () = forward => {}
            _ = stopped => {}
        }
        Ok(())
    });
    Ok(Source {
        serial,
        updates,
        commands,
        stop,
        task,
    })
}
