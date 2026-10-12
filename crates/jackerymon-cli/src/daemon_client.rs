//! A client for a running `jackerymon daemon`.

use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use futures::StreamExt;
use jackerymon::control::Setting;
use tokio::sync::mpsc;

use crate::daemon::EVENT_KEEP_ALIVE;
use crate::record::Update;

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

/// Where a daemon answers.
pub(crate) enum Endpoint {
    /// Base URL, such as `http://127.0.0.1:9888`.
    Tcp(String),
}

/// Requests to one daemon.
pub(crate) struct DaemonClient {
    /// URL that request paths are appended to.
    base: String,
    client: reqwest::Client,
    /// Reads on the event stream time out, so a dead connection is noticed;
    /// a setting may take longer than that, so it uses `client`.
    events: reqwest::Client,
}

impl DaemonClient {
    pub(crate) fn new(endpoint: &Endpoint) -> Result<Self> {
        let Endpoint::Tcp(url) = endpoint;
        Ok(Self {
            base: url.trim_end_matches('/').to_owned(),
            client: reqwest::Client::new(),
            events: reqwest::Client::builder()
                .read_timeout(EVENT_KEEP_ALIVE * MISSED_KEEP_ALIVES)
                .build()
                .context("building the event stream client")?,
        })
    }

    /// The serial of the unit the daemon owns.
    pub(crate) async fn serial(&self) -> Result<String> {
        let status: serde_json::Value = self
            .client
            .get(format!("{}/status", self.base))
            .timeout(DAEMON_STATUS_TIMEOUT)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .with_context(|| format!("reaching the daemon at {}", self.base))?
            .json()
            .await
            .context("reading the daemon's status")?;
        Ok(status["serial"]
            .as_str()
            .context("daemon status has no serial")?
            .to_owned())
    }

    /// Forwards the daemon's updates until the stream fails or ends; returns
    /// why.  Resets `consecutive` once updates flow.
    pub(crate) async fn follow(
        &self,
        sender: &mpsc::UnboundedSender<Update>,
        consecutive: &mut u32,
    ) -> anyhow::Error {
        let response = match self
            .events
            .get(format!("{}/events", self.base))
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

    pub(crate) async fn post_setting(&self, setting: Setting, confirm: bool) -> Result<(), String> {
        let response = self
            .client
            .post(format!("{}/settings?confirm={confirm}", self.base))
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
