//! Where the screen's updates come from and where its settings go.

use std::ops::ControlFlow;

use anyhow::Context;
use anyhow::Result;
use jackerymon::ble;
use jackerymon::control::Setting;
use jackerymon::poll::ControlRequest;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::connection::ConnectionArgs;
use crate::record::Update;

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
    let key_file = connection.key_file()?;
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
