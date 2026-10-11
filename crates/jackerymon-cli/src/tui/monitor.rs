//! What the TUI knows about the unit, built from poll events.

use std::collections::VecDeque;
use std::time::Duration;
use std::time::SystemTime;

use chrono::DateTime;
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
    /// How old the sample already was on arrival: a daemon replays its
    /// latest one to a new client, however old.
    pub(super) age_on_arrival: Duration,
}

impl Reading {
    /// Time since the unit was read.
    pub(super) fn age(&self, now: Instant) -> Duration {
        now.duration_since(self.received) + self.age_on_arrival
    }
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
            // A daemon replays its latest sample to a new client, which
            // may then arrive twice.
            Update::Sample(sample) if self.latest.is_some() && sample.sample == self.samples => {}
            Update::Sample(sample) => {
                self.samples = sample.sample;
                let age_on_arrival = age(&sample.timestamp, SystemTime::now());
                match Status::try_from(sample.status) {
                    Ok(status) => {
                        if let Some(taken) = now.checked_sub(age_on_arrival) {
                            self.record_power(&status, taken, now);
                        }
                        self.invalid = None;
                        self.latest = Some(Reading {
                            status,
                            received: now,
                            age_on_arrival,
                        });
                    }
                    Err(error) => self.invalid = Some(error.to_string()),
                }
            }
        }
    }

    /// Charts a reading taken at `taken`; one older than [`HISTORY`] is
    /// dropped at once.
    fn record_power(&mut self, status: &Status, taken: Instant, now: Instant) {
        if let (Some(input), Some(output)) = (status.input_watts, status.output_watts) {
            self.history.push_back(PowerPoint {
                at: taken,
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

/// How long before `now` a sample's RFC 3339 timestamp is.  An
/// unreadable timestamp, or one ahead of this host's clock, counts as
/// new.
fn age(timestamp: &str, now: SystemTime) -> Duration {
    DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .and_then(|taken| now.duration_since(SystemTime::from(taken)).ok())
        .unwrap_or_default()
}

#[cfg(test)]
pub(super) mod tests {
    use std::time::Duration;
    use std::time::SystemTime;

    use chrono::DateTime;
    use chrono::SecondsFormat;
    use chrono::Utc;
    use serde_json::Value;
    use serde_json::json;
    use tokio::time::Instant;

    use super::HISTORY;
    use super::Link;
    use super::Monitor;
    use crate::record::SampleRecord;
    use crate::record::Update;

    pub(in crate::tui) fn sample(number: u64, status: Value) -> Update {
        sample_taken(number, status, SystemTime::now())
    }

    fn sample_taken(number: u64, status: Value, taken: SystemTime) -> Update {
        Update::Sample(SampleRecord {
            timestamp: DateTime::<Utc>::from(taken).to_rfc3339_opts(SecondsFormat::Micros, false),
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

    #[test]
    fn replayed_sample_is_not_counted_twice() {
        let now = Instant::now();
        let mut monitor = Monitor::new("123456789".to_owned());
        monitor.apply(sample(7, json!({"ip": 1, "op": 1})), now);
        monitor.apply(sample(7, json!({"ip": 1, "op": 1})), now);
        assert_eq!(monitor.history.len(), 1);
        monitor.apply(sample(1, json!({"ip": 2, "op": 2})), now);
        assert_eq!(monitor.history.len(), 2, "a restarted daemon counts from 1");
    }

    #[test]
    fn replayed_old_sample_keeps_its_age() {
        let now = Instant::now();
        let old = Duration::from_secs(2 * 60 * 60);
        let mut monitor = Monitor::new("123456789".to_owned());
        monitor.apply(
            sample_taken(7, json!({"ip": 1, "op": 1}), SystemTime::now() - old),
            now,
        );
        let reading = monitor.latest.as_ref().expect("reading");
        assert!(reading.age(now) >= old);
        assert!(monitor.history.is_empty(), "too old to chart");
    }
}
