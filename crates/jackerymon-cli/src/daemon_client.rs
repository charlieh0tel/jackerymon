//! A client for a running `jackerymon daemon`.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use clap::Args;
use futures::StreamExt;
use jackerymon::control::Setting;
use tokio::sync::mpsc;

use crate::daemon::EVENT_KEEP_ALIVE;
use crate::record::Update;
use crate::runtime;

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

/// Base URL for requests over a Unix socket; the host is not used.
pub(crate) const SOCKET_BASE: &str = "http://localhost";
/// Prefixes that make `--daemon` a URL rather than a socket path.
const URL_SCHEMES: [&str; 2] = ["http://", "https://"];

/// Where a daemon answers.
pub(crate) enum Endpoint {
    /// Base URL, such as `http://127.0.0.1:9888`.
    Tcp(String),
    /// The daemon's Unix socket.
    Unix(PathBuf),
}

impl fmt::Display for Endpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tcp(url) => formatter.write_str(url),
            Self::Unix(path) => write!(formatter, "{}", path.display()),
        }
    }
}

impl Endpoint {
    /// A URL for TCP, anything else a socket path.
    fn parse(text: &str) -> Self {
        if URL_SCHEMES.iter().any(|scheme| text.starts_with(scheme)) {
            Self::Tcp(text.to_owned())
        } else {
            Self::Unix(PathBuf::from(text))
        }
    }
}

/// How a client reaches the unit: through a daemon or directly.
#[derive(Args)]
pub(crate) struct RouteArgs {
    /// Use the daemon at this URL or socket path.  By default a running
    /// daemon's socket is found and used.
    #[arg(long, value_name = "URL_OR_SOCKET", conflicts_with = "direct")]
    daemon: Option<String>,
    /// Connect to the unit directly even if a daemon is running.
    #[arg(long)]
    direct: bool,
}

impl RouteArgs {
    /// The daemon to use for `unit`, the one named on the command line if
    /// any, or `None` to connect directly.
    pub(crate) fn endpoint(&self, unit: Option<&str>) -> Result<Option<Endpoint>> {
        if let Some(daemon) = &self.daemon {
            return Ok(Some(Endpoint::parse(daemon)));
        }
        if self.direct {
            return Ok(None);
        }
        Ok(runtime::find(unit)?.map(Endpoint::Unix))
    }
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
    fn new(endpoint: &Endpoint) -> Result<Self> {
        let (base, socket) = match endpoint {
            Endpoint::Tcp(url) => (url.trim_end_matches('/').to_owned(), None),
            Endpoint::Unix(path) => (SOCKET_BASE.to_owned(), Some(path.clone())),
        };
        let builder = || {
            let builder = reqwest::Client::builder();
            match &socket {
                Some(path) => builder.unix_socket(path.clone()),
                None => builder,
            }
        };
        Ok(Self {
            base,
            client: builder().build().context("building the daemon client")?,
            events: builder()
                .read_timeout(EVENT_KEEP_ALIVE * MISSED_KEEP_ALIVES)
                .build()
                .context("building the event stream client")?,
        })
    }

    /// A client for the daemon at `endpoint`, with the serial of the unit
    /// it owns.  Fails if that is not `expected`, when one is named.
    pub(crate) async fn connect(
        endpoint: &Endpoint,
        expected: Option<&str>,
    ) -> Result<(Self, String)> {
        let client = Self::new(endpoint)?;
        let serial = client.serial().await?;
        if let Some(expected) = expected
            && serial != expected
        {
            bail!("the daemon at {endpoint} owns unit {serial}, not {expected}");
        }
        Ok((client, serial))
    }

    /// The serial of the unit the daemon owns.
    async fn serial(&self) -> Result<String> {
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
