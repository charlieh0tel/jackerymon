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

/// Builds the exposition text, one metric family at a time.
struct Exposition {
    text: String,
    serial: String,
}

impl Exposition {
    fn family(&mut self, name: &str, kind: &str, help: &str) {
        writeln!(self.text, "# HELP {name} {help}").expect("writing to a String");
        writeln!(self.text, "# TYPE {name} {kind}").expect("writing to a String");
    }

    fn sample(&mut self, name: &str, labels: &[(&str, &str)], value: f64) {
        let extra: String = labels
            .iter()
            .map(|(key, label)| format!(",{key}=\"{label}\""))
            .collect();
        writeln!(
            self.text,
            "{name}{{serial=\"{}\"{extra}}} {value}",
            self.serial
        )
        .expect("writing to a String");
    }

    /// A gauge with one value, omitted when there is none.
    fn gauge(&mut self, name: &str, help: &str, value: Option<f64>) {
        if let Some(value) = value {
            self.family(name, "gauge", help);
            self.sample(name, &[], value);
        }
    }

    fn counter(&mut self, name: &str, help: &str, value: u64) {
        self.family(name, "counter", help);
        self.sample(name, &[], to_f64(value));
    }
}

/// Counts stay far below 2^53, where `f64` would lose precision.
fn to_f64(value: u64) -> f64 {
    value as f64
}

pub(super) fn render(snapshot: &Snapshot, now: SystemTime) -> String {
    let mut out = Exposition {
        text: String::new(),
        serial: snapshot.serial.clone(),
    };
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
        return out.text;
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
        readings(&mut out, status);
    }
    out.text
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
    if !outputs.is_empty() {
        out.family("jackery_output_on", "gauge", "1 while an output is on.");
        for (output, on) in outputs {
            out.sample(
                "jackery_output_on",
                &[("output", output)],
                f64::from(u8::from(on)),
            );
        }
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
        let text = render(&snapshot(now), now);
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
            &snapshot(received),
            received + STALE_AFTER + Duration::from_secs(1),
        );
        assert!(text.contains("jackery_last_sample_timestamp_seconds"));
        assert!(!text.contains("jackery_battery_percent"));
    }

    #[test]
    fn nothing_but_counters_before_the_first_sample() {
        let text = render(&Snapshot::new("123456789".to_owned()), SystemTime::now());
        assert!(text.contains("jackery_connected{serial=\"123456789\"} 0\n"));
        assert!(!text.contains("jackery_last_sample"));
    }
}
