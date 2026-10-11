//! What the TUI knows about the unit, built from poll events.

use std::collections::VecDeque;
use std::time::Duration;

use jackerymon::control::Setting;
use jackerymon::status::Status;
use tokio::time::Instant;

use crate::record::Update;

/// How much power history the chart keeps.
pub(super) const HISTORY: Duration = Duration::from_secs(10 * 60);

/// State of the BLE link as the poller last reported it.
#[derive(Debug, PartialEq)]
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

/// Progress of the most recent setting change.
#[derive(Debug, PartialEq)]
pub(super) enum ControlState {
    Pending(Setting),
    Applied(Setting),
    Failed(String),
}

/// One input/output power reading for the chart.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct PowerPoint {
    pub(super) at: Instant,
    pub(super) input_watts: f64,
    pub(super) output_watts: f64,
}

/// The latest decoded status and when it arrived.
#[derive(Debug)]
pub(super) struct Reading {
    pub(super) status: Status,
    pub(super) received: Instant,
}

/// Everything the screen shows.
#[derive(Debug)]
pub(super) struct Monitor {
    pub(super) serial: String,
    pub(super) link: Link,
    pub(super) latest: Option<Reading>,
    /// Status that arrived but did not decode; cleared by a good one.
    pub(super) invalid: Option<String>,
    /// Power readings within [`HISTORY`], oldest first.
    pub(super) history: VecDeque<PowerPoint>,
    pub(super) samples: u64,
    pub(super) transport_failures: u64,
    pub(super) control: Option<ControlState>,
}

impl Monitor {
    pub(super) fn new(serial: String) -> Self {
        Self {
            serial,
            link: Link::Searching,
            latest: None,
            invalid: None,
            history: VecDeque::new(),
            samples: 0,
            transport_failures: 0,
            control: None,
        }
    }

    pub(super) fn apply(&mut self, update: Update, now: Instant) {
        match update {
            Update::Connected { .. } => self.link = Link::Connected,
            Update::Lost { error, consecutive } => {
                self.transport_failures += 1;
                self.link = Link::Lost { error, consecutive };
            }
            Update::Sample(sample) => {
                self.samples = sample.sample;
                match Status::try_from(sample.status) {
                    Ok(status) => {
                        self.record_power(&status, now);
                        self.invalid = None;
                        self.latest = Some(Reading {
                            status,
                            received: now,
                        });
                    }
                    Err(error) => self.invalid = Some(error.to_string()),
                }
            }
        }
    }

    fn record_power(&mut self, status: &Status, now: Instant) {
        if let (Some(input), Some(output)) = (status.input_watts, status.output_watts) {
            self.history.push_back(PowerPoint {
                at: now,
                input_watts: f64::from(input),
                output_watts: f64::from(output),
            });
        }
        while self
            .history
            .front()
            .is_some_and(|point| now.duration_since(point.at) > HISTORY)
        {
            self.history.pop_front();
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::time::Duration;

    use serde_json::Value;
    use serde_json::json;
    use tokio::time::Instant;

    use super::HISTORY;
    use super::Link;
    use super::Monitor;
    use crate::record::SampleRecord;
    use crate::record::Update;

    pub(in crate::tui) fn sample(number: u64, status: Value) -> Update {
        Update::Sample(SampleRecord {
            timestamp: "1970-01-01T00:00:00.000000+00:00".to_owned(),
            sample: number,
            connection: 1,
            transport_failures: 0,
            query_ms: 220.0,
            status: status.as_object().expect("object").clone(),
        })
    }

    #[test]
    fn tracks_link_and_latest_status() {
        let now = Instant::now();
        let mut monitor = Monitor::new("123456789".to_owned());
        monitor.apply(Update::Connected { connection: 1 }, now);
        assert_eq!(monitor.link, Link::Connected);
        monitor.apply(sample(1, json!({"rb": 90, "ip": 5, "op": 7})), now);
        let reading = monitor.latest.as_ref().expect("reading");
        assert_eq!(reading.status.battery_percent, Some(90));
        assert_eq!(monitor.history.len(), 1);
        monitor.apply(
            Update::Lost {
                error: "device disconnected".to_owned(),
                consecutive: 1,
            },
            now,
        );
        assert!(matches!(monitor.link, Link::Lost { consecutive: 1, .. }));
        assert_eq!(monitor.transport_failures, 1);
        assert!(monitor.latest.is_some(), "last reading survives a dropout");
    }

    #[test]
    fn invalid_status_keeps_last_good_reading() {
        let now = Instant::now();
        let mut monitor = Monitor::new("123456789".to_owned());
        monitor.apply(sample(1, json!({"rb": 90})), now);
        monitor.apply(sample(2, json!({"rb": "bad"})), now);
        assert!(monitor.invalid.is_some());
        assert_eq!(
            monitor
                .latest
                .as_ref()
                .expect("reading")
                .status
                .battery_percent,
            Some(90)
        );
        monitor.apply(sample(3, json!({"rb": 80})), now);
        assert!(monitor.invalid.is_none());
    }

    #[test]
    fn history_drops_old_points() {
        let start = Instant::now();
        let mut monitor = Monitor::new("123456789".to_owned());
        monitor.apply(sample(1, json!({"ip": 1, "op": 1})), start);
        let later = start + HISTORY + Duration::from_secs(1);
        monitor.apply(sample(2, json!({"ip": 2, "op": 2})), later);
        assert_eq!(monitor.history.len(), 1);
        assert_eq!(monitor.history[0].input_watts, 2.0);
    }
}
