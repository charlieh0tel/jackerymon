//! Daily JSON-lines sample log: one `YYYY-MM-DD.jsonl` file per UTC day.

use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use chrono::Utc;

use crate::record::SampleRecord;

/// Length of the `YYYY-MM-DD` prefix of an RFC 3339 timestamp.
const DATE_LENGTH: usize = 10;
/// `chrono` format of that prefix.
const DATE_FORMAT: &str = "%Y-%m-%d";

/// Appends samples to the file for their day, switching at midnight UTC.
pub(super) struct SampleLog {
    directory: PathBuf,
    /// The open file and the day it holds.
    current: Option<(String, File)>,
}

impl SampleLog {
    /// Opens today's file, so an unusable directory fails at startup
    /// rather than on every sample.
    pub(super) fn new(directory: PathBuf) -> Result<Self> {
        let day = Utc::now().format(DATE_FORMAT).to_string();
        let file = open(&directory, &day)?;
        Ok(Self {
            directory,
            current: Some((day, file)),
        })
    }

    pub(super) fn write(&mut self, record: &SampleRecord) -> Result<()> {
        let day = record
            .timestamp
            .get(..DATE_LENGTH)
            .context("sample timestamp has no date")?;
        let file = match &mut self.current {
            Some((open_day, file)) if open_day == day => file,
            current => {
                &mut current
                    .insert((day.to_owned(), open(&self.directory, day)?))
                    .1
            }
        };
        writeln!(file, "{record}").context("writing the sample log")
    }
}

/// Opens the file for `day` to append to.
fn open(directory: &Path, day: &str) -> Result<File> {
    let path = directory.join(format!("{day}.jsonl"));
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::SampleLog;
    use crate::record::SampleRecord;

    fn record(timestamp: &str, sample: u64) -> SampleRecord {
        SampleRecord {
            timestamp: timestamp.to_owned(),
            sample,
            connection: 1,
            transport_failures: 0,
            query_ms: 216.2,
            status: json!({"rb": 100}).as_object().expect("object").clone(),
        }
    }

    #[test]
    fn appends_per_utc_day() {
        let directory = std::env::temp_dir().join(format!("jackerymon-log-{}", std::process::id()));
        fs::create_dir_all(&directory).expect("temp dir");
        let mut log = SampleLog::new(directory.clone()).expect("open");
        log.write(&record("2026-10-11T23:59:59.000000+00:00", 1))
            .expect("write");
        log.write(&record("2026-10-12T00:00:04.000000+00:00", 2))
            .expect("write");
        log.write(&record("2026-10-12T00:00:09.000000+00:00", 3))
            .expect("write");
        let first = fs::read_to_string(directory.join("2026-10-11.jsonl")).expect("first day");
        let second = fs::read_to_string(directory.join("2026-10-12.jsonl")).expect("second day");
        fs::remove_dir_all(&directory).expect("cleanup");
        assert_eq!(first.lines().count(), 1);
        assert_eq!(second.lines().count(), 2);
        assert!(
            second
                .lines()
                .all(|line| line.starts_with("{\"timestamp\""))
        );
    }

    #[test]
    fn missing_directory_fails_at_once() {
        let directory = std::env::temp_dir()
            .join("jackerymon-log-missing")
            .join("nested");
        assert!(SampleLog::new(directory).is_err());
    }
}
