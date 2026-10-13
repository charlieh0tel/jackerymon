//! The browser view's data: the latest reading worded as the TUI words
//! it, and chart series thinned from the sample log.  The pages
//! themselves are files beside this one, built into the binary.

use std::path::Path;

use anyhow::Result;
use chrono::DateTime;
use chrono::Utc;
use jackerymon::status::Status;
use serde::Serialize;

use super::log;
use crate::record::SampleRecord;
use crate::text::battery_protection_text;
use crate::text::battery_state_text;
use crate::text::charge_speed_text;
use crate::text::fault_text;
use crate::text::light_text;
use crate::text::on_off;
use crate::text::screen_text;
use crate::text::temperature_alarm_text;
use crate::text::timer_text;

pub(super) const INDEX: &str = include_str!("web/index.html");
pub(super) const STYLE: &str = include_str!("web/style.css");
pub(super) const COMMON: &str = include_str!("web/common.js");

/// Most points a series returns; `every` is raised to keep under it.
const MAX_POINTS: i64 = 5_000;

/// One reading as the page shows it.  Numbers stay numbers; settings are
/// words, the same words as the TUI's.
#[derive(Debug, Serialize)]
pub(super) struct View {
    battery_percent: Option<u8>,
    battery_celsius: Option<f64>,
    battery_state: Option<String>,
    input_watts: Option<u32>,
    output_watts: Option<u32>,
    ac_input_watts: Option<u32>,
    car_input_watts: Option<u32>,
    solar_input_watts: Option<f64>,
    hours_to_full: Option<f64>,
    hours_to_empty: Option<f64>,
    ac_output_volts: Option<f64>,
    ac_output_hertz: Option<u16>,
    ac_output: Option<String>,
    dc_output: Option<String>,
    usb_output: Option<String>,
    car_output: Option<String>,
    light: Option<String>,
    screen: Option<String>,
    auto_off: Option<String>,
    energy_saving: Option<String>,
    charge_speed: Option<String>,
    battery_protection: Option<String>,
    emergency_charge: Option<String>,
    ups: Option<String>,
    fault: Option<String>,
    temperature_alarm: Option<String>,
}

impl From<&Status> for View {
    fn from(status: &Status) -> Self {
        Self {
            battery_percent: status.battery_percent,
            battery_celsius: status.battery_celsius,
            battery_state: status.battery_state.map(battery_state_text),
            input_watts: status.input_watts,
            output_watts: status.output_watts,
            ac_input_watts: status.ac_input_watts,
            car_input_watts: status.car_input_watts,
            solar_input_watts: status.solar_input_watts,
            // The unit reports 0 h to full while nothing charges.
            hours_to_full: status
                .input_hours_remaining
                .filter(|_| status.input_watts != Some(0)),
            hours_to_empty: status.output_hours_remaining,
            ac_output_volts: status.ac_output_volts,
            ac_output_hertz: status.ac_output_hertz,
            ac_output: status.ac_output.map(on_off),
            dc_output: status.dc_output.map(on_off),
            usb_output: status.usb_output.map(on_off),
            car_output: status.car_output.map(on_off),
            light: status.light_mode.map(light_text),
            screen: status.screen.map(screen_text),
            auto_off: status.auto_off.map(timer_text),
            energy_saving: status.energy_saving.map(timer_text),
            charge_speed: status.charge_speed.map(charge_speed_text),
            battery_protection: status.battery_protection.map(battery_protection_text),
            emergency_charge: status.emergency_charge.map(on_off),
            ups: status.ups.map(on_off),
            fault: status.error_code.map(fault_text),
            temperature_alarm: status.temperature_alarm.map(temperature_alarm_text),
        }
    }
}

/// Chart columns, as uPlot takes them: one time axis and a value per
/// series at each time.  A bucket with no value for a series is `null`.
#[derive(Debug, Default, PartialEq, Serialize)]
pub(super) struct Series {
    /// Unix seconds, the start of each bucket.
    time: Vec<i64>,
    input_watts: Vec<Option<f64>>,
    output_watts: Vec<Option<f64>>,
    battery_percent: Vec<Option<f64>>,
    battery_celsius: Vec<Option<f64>>,
    /// The bucket width used, in seconds, after any raise to stay under
    /// the point limit.
    every: i64,
}

/// Running sums for one bucket.
#[derive(Default)]
struct Bucket {
    start: i64,
    input_watts: Mean,
    output_watts: Mean,
    battery_percent: Mean,
    battery_celsius: Mean,
}

/// A mean built one value at a time.
#[derive(Default)]
struct Mean {
    sum: f64,
    count: u32,
}

impl Mean {
    fn add(&mut self, value: Option<f64>) {
        if let Some(value) = value {
            self.sum += value;
            self.count += 1;
        }
    }

    fn value(&self) -> Option<f64> {
        (self.count > 0).then(|| self.sum / f64::from(self.count))
    }
}

impl Series {
    fn push(&mut self, bucket: &Bucket) {
        self.time.push(bucket.start);
        self.input_watts.push(bucket.input_watts.value());
        self.output_watts.push(bucket.output_watts.value());
        self.battery_percent.push(bucket.battery_percent.value());
        self.battery_celsius.push(bucket.battery_celsius.value());
    }
}

/// Logged samples in `since..=until`, averaged over buckets `every`
/// seconds wide, widened if the range would need more than
/// [`MAX_POINTS`].
pub(super) fn series(
    directory: &Path,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    every: i64,
) -> Result<Series> {
    let span = (until - since).num_seconds().max(1);
    // The span is positive, so this rounds up.
    let every = every.max(1).max((span + MAX_POINTS - 1) / MAX_POINTS);
    let mut series = Series {
        every,
        ..Series::default()
    };
    let mut bucket: Option<Bucket> = None;
    log::read_history(directory, since, until, |line| {
        let Ok(record) = serde_json::from_str::<SampleRecord>(line) else {
            return true;
        };
        let (Ok(taken), Ok(status)) = (
            DateTime::parse_from_rfc3339(&record.timestamp),
            Status::try_from(record.status),
        ) else {
            return true;
        };
        let seconds = taken.timestamp();
        let start = seconds - seconds.rem_euclid(every);
        if bucket.as_ref().is_some_and(|bucket| bucket.start != start) {
            series.push(&bucket.take().expect("checked"));
        }
        let current = bucket.get_or_insert_with(|| Bucket {
            start,
            ..Bucket::default()
        });
        current.input_watts.add(status.input_watts.map(f64::from));
        current.output_watts.add(status.output_watts.map(f64::from));
        current
            .battery_percent
            .add(status.battery_percent.map(f64::from));
        current.battery_celsius.add(status.battery_celsius);
        true
    })?;
    if let Some(bucket) = bucket {
        series.push(&bucket);
    }
    Ok(series)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use chrono::DateTime;
    use serde_json::json;

    use super::series;
    use crate::record::SampleRecord;

    #[test]
    fn series_averages_each_bucket() {
        let directory =
            std::env::temp_dir().join(format!("jackerymon-series-{}", std::process::id()));
        fs::create_dir_all(&directory).expect("temp dir");
        let line = |second: u32, ip: u32, rb: u32| {
            SampleRecord {
                timestamp: format!("2026-10-11T12:00:{second:02}.000000+00:00"),
                sample: u64::from(second),
                connection: 1,
                transport_failures: 0,
                query_ms: 1.0,
                status: json!({"ip": ip, "op": 0, "rb": rb, "bt": 250})
                    .as_object()
                    .expect("object")
                    .clone(),
            }
            .to_string()
        };
        let lines = [line(0, 10, 90), line(5, 30, 92), line(10, 50, 94)].join("\n");
        fs::write(directory.join("2026-10-11.jsonl"), format!("{lines}\n")).expect("log");
        let at = |text: &str| DateTime::parse_from_rfc3339(text).expect("time").to_utc();
        let series = series(
            &directory,
            at("2026-10-11T12:00:00Z"),
            at("2026-10-11T12:01:00Z"),
            10,
        )
        .expect("series");
        fs::remove_dir_all(&directory).expect("cleanup");
        assert_eq!(series.every, 10);
        assert_eq!(series.time.len(), 2, "two 10 s buckets");
        assert_eq!(series.input_watts, [Some(20.0), Some(50.0)]);
        assert_eq!(series.battery_percent, [Some(91.0), Some(94.0)]);
        assert_eq!(series.battery_celsius, [Some(25.0), Some(25.0)]);
    }

    #[test]
    fn long_ranges_are_thinned_to_the_point_limit() {
        let directory = std::env::temp_dir();
        let at = |text: &str| DateTime::parse_from_rfc3339(text).expect("time").to_utc();
        let week = series(
            &directory.join("jackerymon-series-none"),
            at("2026-10-01T00:00:00Z"),
            at("2026-10-08T00:00:00Z"),
            1,
        );
        // No log files: nothing read, but the width is still chosen.  A
        // week over the limit is 604,800 s / 5,000, rounded up.
        assert_eq!(week.expect("series").every, 121);
    }
}
