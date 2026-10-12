//! Where the screen's updates come from and where its settings go.

use std::ops::ControlFlow;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use futures::StreamExt;
use jackerymon::ble;
use jackerymon::control::Setting;
use jackerymon::poll::ControlRequest;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::sleep;

use crate::connection::ConnectionArgs;
use crate::daemon::EVENT_KEEP_ALIVE;
use crate::record::Update;

/// Delay before reconnecting to a daemon's event stream.
const DAEMON_RETRY_DELAY: Duration = Duration::from_secs(2);
/// How long to wait for the daemon's status at startup.
const DAEMON_STATUS_TIMEOUT: Duration = Duration::from_secs(5);
/// How long to wait for a setting: a little over the daemon's own limit,
/// so its answer arrives first.
const DAEMON_SETTING_TIMEOUT: Duration = Duration::from_secs(70);
/// Keep-alives the event stream may miss before it counts as dead.
const MISSED_KEEP_ALIVES: u32 = 3;
/// Separates server-sent events.
const EVENT_SEPARATOR: &[u8] = b"\n\n";
const EVENT_DATA_PREFIX: &str = "data:";

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

/// Follows a running `jackerymon daemon` at `url`.  The daemon owns the
/// unit; this only reads its events and posts settings to it.
pub(super) async fn daemon(url: &str) -> Result<Source> {
    let url = url.trim_end_matches('/').to_owned();
    let client = reqwest::Client::new();
    // Reads on the event stream time out, so a dead connection is noticed;
    // a setting may take longer than that, so it uses `client`.
    let events_client = reqwest::Client::builder()
        .read_timeout(EVENT_KEEP_ALIVE * MISSED_KEEP_ALIVES)
        .build()
        .context("building the event stream client")?;
    let status: serde_json::Value = client
        .get(format!("{url}/status"))
        .timeout(DAEMON_STATUS_TIMEOUT)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .with_context(|| format!("reaching the daemon at {url}"))?
        .json()
        .await
        .context("reading the daemon's status")?;
    let serial = status["serial"]
        .as_str()
        .context("daemon status has no serial")?
        .to_owned();
    let (sender, updates) = mpsc::unbounded_channel();
    let (commands, mut asked) = mpsc::channel::<Command>(1);
    let (stop, stopped) = oneshot::channel::<()>();
    let follow = {
        let url = url.clone();
        async move {
            let mut consecutive = 0;
            loop {
                let error = follow_events(&events_client, &url, &sender, &mut consecutive).await;
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
            let result = post_setting(&client, &url, command.setting).await;
            command.reply.send(result).ok();
        }
    };
    let task = tokio::spawn(async move {
        tokio::select! {
            () = follow => {}
            // The screen confirmed any warning before sending.
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

/// Forwards the daemon's updates until the stream fails or ends; returns
/// why.  Resets `consecutive` once updates flow.
async fn follow_events(
    client: &reqwest::Client,
    url: &str,
    sender: &mpsc::UnboundedSender<Update>,
    consecutive: &mut u32,
) -> anyhow::Error {
    let response = match client
        .get(format!("{url}/events"))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
    {
        Ok(response) => response,
        Err(error) => return error.into(),
    };
    let mut chunks = response.bytes_stream();
    let mut buffer = Vec::new();
    while let Some(chunk) = chunks.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => return error.into(),
        };
        buffer.extend_from_slice(&chunk);
        for update in drain_events(&mut buffer) {
            *consecutive = 0;
            if sender.send(update).is_err() {
                return anyhow!("screen closed");
            }
        }
    }
    anyhow!("event stream ended")
}

/// Takes the complete events off the front of `buffer`, leaving a partial
/// one.  Splitting on bytes keeps a character cut between chunks whole.
fn drain_events(buffer: &mut Vec<u8>) -> Vec<Update> {
    let mut updates = Vec::new();
    while let Some(end) = buffer
        .windows(EVENT_SEPARATOR.len())
        .position(|window| window == EVENT_SEPARATOR)
    {
        let event: Vec<u8> = buffer.drain(..end + EVENT_SEPARATOR.len()).collect();
        updates.extend(parse_event(&String::from_utf8_lossy(&event)));
    }
    updates
}

/// The updates in one server-sent event; malformed data is skipped.
fn parse_event(event: &str) -> Vec<Update> {
    event
        .lines()
        .filter_map(|line| line.strip_prefix(EVENT_DATA_PREFIX))
        .filter_map(|data| serde_json::from_str(data.trim()).ok())
        .collect()
}

async fn post_setting(client: &reqwest::Client, url: &str, setting: Setting) -> Result<(), String> {
    let response = client
        .post(format!("{url}/settings?confirm=true"))
        .timeout(DAEMON_SETTING_TIMEOUT)
        .json(&setting.body())
        .send()
        .await
        .map_err(|error| format!("{setting}: daemon unreachable: {error}"))?;
    if response.status().is_success() {
        return Ok(());
    }
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap_or_default();
    Err(body["error"].as_str().map_or_else(
        || format!("{setting}: daemon answered {status}"),
        str::to_owned,
    ))
}

#[cfg(test)]
mod tests {
    use super::drain_events;
    use super::parse_event;
    use crate::record::Update;

    #[test]
    fn parses_server_sent_event_data() {
        let event = "data: {\"type\":\"connected\",\"connection\":2}\n\n";
        assert_eq!(parse_event(event), [Update::Connected { connection: 2 }]);
        assert!(parse_event(": keep-alive\n\n").is_empty());
        assert!(parse_event("data: not json\n\n").is_empty());
    }

    #[test]
    fn character_split_between_chunks_survives() {
        let event = "data: {\"type\":\"lost\",\"error\":\"searching…\",\"consecutive\":1}\n\n";
        let bytes = event.as_bytes();
        let split = event.find('…').expect("ellipsis") + 1;
        let mut buffer = bytes[..split].to_vec();
        assert!(drain_events(&mut buffer).is_empty());
        buffer.extend_from_slice(&bytes[split..]);
        assert_eq!(
            drain_events(&mut buffer),
            [Update::Lost {
                error: "searching…".to_owned(),
                consecutive: 1
            }]
        );
        assert!(buffer.is_empty());
    }
}
