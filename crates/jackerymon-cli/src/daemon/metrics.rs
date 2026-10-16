//! Prometheus text exposition of the daemon's snapshot.
//!
//! Readings older than [`STALE_AFTER`] are left out, so a unit that has
//! gone quiet never looks live; `jackery_connected` and
//! `jackery_last_sample_timestamp_seconds` say what happened.

use std::fmt::Write;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use jackerymon::status::Status;

use super::state::Link;
use super::state::Snapshot;

pub(super) const STALE_AFTER: Duration = Duration::from_secs(60);

/// One metric family: its header and a line per sample, from every unit.
struct Family {
    name: &'static str,
    kind: &'static str,
    help: &'static str,
    samples: Vec<String>,
}

/// Builds the exposition text.  Families are written in the order first
/// seen, each header once, however many units have a sample in it.
struct Exposition {
    families: Vec<Family>,
    /// The unit whose samples are being added.
    serial: String,
}

impl Exposition {
    fn family(
        &mut self,
        name: &'static str,
        kind: &'static str,
        help: &'static str,
    ) -> &mut Family {
        let at = match self.families.iter().position(|family| family.name == name) {
            Some(at) => at,
            None => {
                self.families.push(Family {
                    name,
                    kind,
                    help,
                    samples: Vec::new(),
                });
                self.families.len() - 1
            }
        };
        &mut self.families[at]
    }

    fn sample(
        &mut self,
        (name, kind, help): (&'static str, &'static str, &'static str),
        labels: &[(&str, &str)],
        value: f64,
    ) {
        let extra: String = labels
            .iter()
            .map(|(key, label)| format!(",{key}=\"{label}\""))
            .collect();
        let line = format!("{name}{{serial=\"{}\"{extra}}} {value}", self.serial);
        self.family(name, kind, help).samples.push(line);
    }

    /// A gauge with one value, omitted when there is none.
    fn gauge(&mut self, name: &'static str, help: &'static str, value: Option<f64>) {
        if let Some(value) = value {
            self.sample((name, GAUGE, help), &[], value);
        }
    }

    fn counter(&mut self, name: &'static str, help: &'static str, value: u64) {
        self.sample((name, COUNTER, help), &[], to_f64(value));
    }

    fn text(&self) -> String {
        let mut text = String::new();
        for family in &self.families {
            let Family {
                name,
                kind,
                help,
                samples,
            } = family;
            writeln!(text, "# HELP {name} {help}\n# TYPE {name} {kind}")
                .expect("writing to a String");
            for sample in samples {
                writeln!(text, "{sample}").expect("writing to a String");
            }
        }
        text
    }
}

const GAUGE: &str = "gauge";
const COUNTER: &str = "counter";

/// Counts stay far below 2^53, where `f64` would lose precision.
fn to_f64(value: u64) -> f64 {
    value as f64
}

/// The exposition of every unit in `snapshots`.
pub(super) fn render(snapshots: &[Snapshot], now: SystemTime) -> String {
    let mut out = Exposition {
        families: Vec::new(),
        serial: String::new(),
    };
    for snapshot in snapshots {
        out.serial.clone_from(&snapshot.serial);
        unit(&mut out, snapshot, now);
    }
    out.text()
}

fn unit(out: &mut Exposition, snapshot: &Snapshot, now: SystemTime) {
    let connected = matches!(snapshot.link, Link::Connected);
    out.gauge(
        "jackery_connected",
        "1 while the BLE link to the unit is up.",
        Some(f64::from(u8::from(connected))),
    );
    out.counter(
        "jackery_samples_total",
        "Status responses received.",
        snapshot.samples,
    );
    out.counter(
        "jackery_connections_total",
        "BLE connections established.",
        snapshot.connections,
    );
    out.counter(
        "jackery_transport_failures_total",
        "BLE transport failures; each is followed by a reconnect.",
        snapshot.transport_failures,
    );
    let Some(latest) = &snapshot.latest else {
        return;
    };
    out.gauge(
        "jackery_last_sample_timestamp_seconds",
        "When the latest status response arrived, in Unix seconds.",
        latest
            .received
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|since| since.as_secs_f64()),
    );
    let fresh = now
        .duration_since(latest.received)
        .is_ok_and(|age| age <= STALE_AFTER);
    if let (true, Some(status)) = (fresh, &latest.status) {
        readings(out, status);
    }
}

fn readings(out: &mut Exposition, status: &Status) {
    let watts = |value: Option<u32>| value.map(f64::from);
    out.gauge(
        "jackery_battery_percent",
        "Remaining battery charge.",
        status.battery_percent.map(f64::from),
    );
    out.gauge(
        "jackery_battery_temperature_celsius",
        "Battery temperature.",
        status.battery_celsius,
    );
    out.gauge(
        "jackery_input_watts",
        "Total input power.",
        watts(status.input_watts),
    );
    out.gauge(
        "jackery_output_watts",
        "Total output power.",
        watts(status.output_watts),
    );
    out.gauge(
        "jackery_ac_input_watts",
        "AC input power.",
        watts(status.ac_input_watts),
    );
    out.gauge(
        "jackery_car_input_watts",
        "Car (12 V) input power.",
        watts(status.car_input_watts),
    );
    out.gauge(
        "jackery_solar_input_watts",
        "Solar input power.",
        status.solar_input_watts,
    );
    out.gauge(
        "jackery_input_hours_remaining",
        "Unit's estimate of hours to full.",
        status.input_hours_remaining,
    );
    out.gauge(
        "jackery_output_hours_remaining",
        "Unit's estimate of hours to empty.",
        status.output_hours_remaining,
    );
    out.gauge(
        "jackery_ac_output_volts",
        "AC output voltage.",
        status.ac_output_volts,
    );
    out.gauge(
        "jackery_ac_output_hertz",
        "AC output frequency.",
        status.ac_output_hertz.map(f64::from),
    );
    out.gauge(
        "jackery_error_code",
        "Unit fault code; 0 means none, n shows as F(n-1) in the app.",
        status.error_code.map(|code| code as f64),
    );
    let outputs: Vec<(&str, bool)> = [
        ("ac", status.ac_output),
        ("dc", status.dc_output),
        ("usb", status.usb_output),
        ("car", status.car_output),
    ]
    .into_iter()
    .filter_map(|(output, state)| state.map(|on| (output, on)))
    .collect();
    for (output, on) in outputs {
        out.sample(
            ("jackery_output_on", GAUGE, "1 while an output is on."),
            &[("output", output)],
            f64::from(u8::from(on)),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;
    use std::time::SystemTime;

    use serde_json::json;

    use super::STALE_AFTER;
    use super::render;
    use crate::daemon::state::Snapshot;
    use crate::daemon::state::tests::sample;
    use crate::record::Update;

    fn snapshot(received: SystemTime) -> Snapshot {
        let mut snapshot = Snapshot::new("123456789".to_owned());
        snapshot.apply(&Update::Connected { connection: 1 }, received);
        snapshot.apply(
            &sample(
                3,
                json!({"rb": 100, "bt": 279, "ip": 0, "op": 2, "oac": 0, "odc": 1, "ec": 0}),
            ),
            received,
        );
        snapshot
    }

    #[test]
    fn fresh_readings_are_exported() {
        let now = SystemTime::now();
        let text = render(&[snapshot(now)], now);
        for expected in [
            "# TYPE jackery_connected gauge\njackery_connected{serial=\"123456789\"} 1\n",
            "jackery_samples_total{serial=\"123456789\"} 3\n",
            "jackery_battery_percent{serial=\"123456789\"} 100\n",
            "jackery_battery_temperature_celsius{serial=\"123456789\"} 27.9\n",
            "jackery_output_on{serial=\"123456789\",output=\"ac\"} 0\n",
            "jackery_output_on{serial=\"123456789\",output=\"dc\"} 1\n",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in\n{text}");
        }
        assert!(!text.contains("output=\"usb\""));
    }

    #[test]
    fn stale_readings_are_left_out() {
        let received = SystemTime::now();
        let text = render(
            &[snapshot(received)],
            received + STALE_AFTER + Duration::from_secs(1),
        );
        assert!(text.contains("jackery_last_sample_timestamp_seconds"));
        assert!(!text.contains("jackery_battery_percent"));
    }

    #[test]
    fn each_family_is_declared_once_for_all_units() {
        let now = SystemTime::now();
        let mut other = snapshot(now);
        other.serial = "987654321".to_owned();
        let text = render(&[snapshot(now), other], now);
        assert_eq!(
            text.matches("# TYPE jackery_battery_percent gauge").count(),
            1
        );
        assert!(text.contains(
            "jackery_battery_percent{serial=\"123456789\"} 100\n\
             jackery_battery_percent{serial=\"987654321\"} 100\n"
        ));
        assert_eq!(text.matches("# TYPE jackery_output_on gauge").count(), 1);
    }

    #[test]
    fn nothing_but_counters_before_the_first_sample() {
        let text = render(&[Snapshot::new("123456789".to_owned())], SystemTime::now());
        assert!(text.contains("jackery_connected{serial=\"123456789\"} 0\n"));
        assert!(!text.contains("jackery_last_sample"));
    }
}
