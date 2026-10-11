//! What the daemon knows about the unit, shared by the poller and HTTP.

use std::time::SystemTime;

use jackerymon::status::Status;
use serde::Serialize;

use crate::record::SampleRecord;
use crate::record::Update;

/// State of the BLE link as the poller last reported it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum Link {
    /// No connection yet.
    Searching,
    Connected,
    /// Reconnecting after a transport failure.
    Lost {
        error: String,
        consecutive: u32,
    },
}

/// The latest sample, decoded when its status is valid.
#[derive(Clone, Debug)]
pub(super) struct Latest {
    pub(super) record: SampleRecord,
    pub(super) status: Option<Status>,
    pub(super) received: SystemTime,
}

/// Everything `/status` and `/metrics` report.
#[derive(Clone, Debug)]
pub(super) struct Snapshot {
    pub(super) serial: String,
    pub(super) link: Link,
    pub(super) latest: Option<Latest>,
    pub(super) samples: u64,
    pub(super) connections: u64,
    pub(super) transport_failures: u64,
}

impl Snapshot {
    pub(super) fn new(serial: String) -> Self {
        Self {
            serial,
            link: Link::Searching,
            latest: None,
            samples: 0,
            connections: 0,
            transport_failures: 0,
        }
    }

    pub(super) fn apply(&mut self, update: &Update, now: SystemTime) {
        match update {
            Update::Connected { connection } => {
                self.connections = *connection;
                self.link = Link::Connected;
            }
            Update::Lost { error, consecutive } => {
                self.transport_failures += 1;
                self.link = Link::Lost {
                    error: error.clone(),
                    consecutive: *consecutive,
                };
            }
            Update::Sample(record) => {
                self.samples = record.sample;
                self.latest = Some(Latest {
                    record: record.clone(),
                    status: Status::try_from(record.status.clone()).ok(),
                    received: now,
                });
            }
        }
    }

    /// Updates that bring a new `/events` client up to date.
    pub(super) fn replay(&self) -> Vec<Update> {
        let link = match &self.link {
            Link::Searching => None,
            Link::Connected => Some(Update::Connected {
                connection: self.connections,
            }),
            Link::Lost { error, consecutive } => Some(Update::Lost {
                error: error.clone(),
                consecutive: *consecutive,
            }),
        };
        link.into_iter()
            .chain(
                self.latest
                    .as_ref()
                    .map(|latest| Update::Sample(latest.record.clone())),
            )
            .collect()
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::time::SystemTime;

    use serde_json::Value;
    use serde_json::json;

    use super::Link;
    use super::Snapshot;
    use crate::record::SampleRecord;
    use crate::record::Update;

    pub(in crate::daemon) fn sample(number: u64, status: Value) -> Update {
        Update::Sample(SampleRecord {
            timestamp: "2026-10-11T00:00:00.000000+00:00".to_owned(),
            sample: number,
            connection: 1,
            transport_failures: 0,
            query_ms: 216.2,
            status: status.as_object().expect("object").clone(),
        })
    }

    #[test]
    fn tracks_link_samples_and_replays() {
        let now = SystemTime::now();
        let mut snapshot = Snapshot::new("123456789".to_owned());
        assert!(snapshot.replay().is_empty());
        snapshot.apply(&Update::Connected { connection: 1 }, now);
        snapshot.apply(&sample(1, json!({"rb": 90})), now);
        let latest = snapshot.latest.as_ref().expect("latest");
        assert_eq!(
            latest
                .status
                .as_ref()
                .and_then(|status| status.battery_percent),
            Some(90)
        );
        assert_eq!(
            snapshot.replay(),
            [
                Update::Connected { connection: 1 },
                sample(1, json!({"rb": 90}))
            ]
        );
        let lost = Update::Lost {
            error: "device disconnected".to_owned(),
            consecutive: 1,
        };
        snapshot.apply(&lost, now);
        assert!(matches!(snapshot.link, Link::Lost { consecutive: 1, .. }));
        assert_eq!(snapshot.transport_failures, 1);
        assert_eq!(snapshot.replay()[0], lost);
    }
}
