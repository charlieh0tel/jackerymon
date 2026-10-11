//! JSON shapes shared by `poll` output, daemon logs, and the daemon's
//! HTTP API, and consumed by the TUI.

use std::fmt;

use chrono::DateTime;
use chrono::SecondsFormat;
use chrono::Utc;
use jackerymon::frame::RawStatus;
use jackerymon::poll::PollEvent;
use jackerymon::poll::Sample;
use serde::Deserialize;
use serde::Serialize;

const MILLIS_PER_SECOND: f64 = 1000.0;
/// `query_ms` is rounded to tenths, as the Python PoC does.
const QUERY_MS_STEPS_PER_MILLI: f64 = 10.0;

/// One sample in the Python PoC's `--poll` line shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct SampleRecord {
    /// RFC 3339, UTC, microseconds.
    pub(crate) timestamp: String,
    pub(crate) sample: u64,
    pub(crate) connection: u64,
    pub(crate) transport_failures: u64,
    pub(crate) query_ms: f64,
    pub(crate) status: RawStatus,
}

impl From<&Sample> for SampleRecord {
    fn from(sample: &Sample) -> Self {
        Self {
            timestamp: DateTime::<Utc>::from(sample.timestamp)
                .to_rfc3339_opts(SecondsFormat::Micros, false),
            sample: sample.sample,
            connection: sample.connection,
            transport_failures: sample.transport_failures,
            query_ms: (sample.query.as_secs_f64() * MILLIS_PER_SECOND * QUERY_MS_STEPS_PER_MILLI)
                .round()
                / QUERY_MS_STEPS_PER_MILLI,
            status: sample.status.clone(),
        }
    }
}

/// The JSON line.
impl fmt::Display for SampleRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&serde_json::to_string(self).map_err(|_| fmt::Error)?)
    }
}

/// A change the TUI shows, whether it came from BLE or the daemon.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Update {
    Connected {
        connection: u64,
    },
    Sample(SampleRecord),
    /// Reconnecting after a transport failure.
    Lost {
        error: String,
        consecutive: u32,
    },
}

impl From<PollEvent> for Update {
    fn from(event: PollEvent) -> Self {
        match event {
            PollEvent::Connected { connection } => Self::Connected { connection },
            PollEvent::Sample(sample) => Self::Sample(SampleRecord::from(&sample)),
            PollEvent::TransportFailure { error, consecutive } => Self::Lost {
                error: error.to_string(),
                consecutive,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;
    use std::time::SystemTime;

    use jackerymon::poll::Sample;
    use serde_json::json;

    use super::SampleRecord;
    use super::Update;

    #[test]
    fn sample_line_matches_poc_shape() {
        let sample = Sample {
            timestamp: SystemTime::UNIX_EPOCH + Duration::from_micros(1_500_000),
            sample: 2,
            connection: 1,
            transport_failures: 0,
            query: Duration::from_micros(216_249),
            status: json!({"rb": 100}).as_object().expect("object").clone(),
        };
        let line = SampleRecord::from(&sample).to_string();
        assert_eq!(
            line,
            r#"{"timestamp":"1970-01-01T00:00:01.500000+00:00","sample":2,"connection":1,"transport_failures":0,"query_ms":216.2,"status":{"rb":100}}"#
        );
    }

    #[test]
    fn updates_round_trip() {
        let update = Update::Lost {
            error: "device disconnected".to_owned(),
            consecutive: 2,
        };
        let text = serde_json::to_string(&update).expect("json");
        assert_eq!(
            text,
            r#"{"type":"lost","error":"device disconnected","consecutive":2}"#
        );
        assert_eq!(
            serde_json::from_str::<Update>(&text).expect("parse"),
            update
        );
    }
}
