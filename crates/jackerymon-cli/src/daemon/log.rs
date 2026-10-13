//! Daily JSON-lines sample log: one `YYYY-MM-DD.jsonl` file per UTC day.
//! `jackerymon compress-logs` gzips the finished days; the daemon itself
//! never compresses or deletes.

use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use chrono::Utc;
use clap::Args;
use flate2::Compression;
use flate2::write::GzEncoder;

use crate::record::SampleRecord;

/// Length of the `YYYY-MM-DD` prefix of an RFC 3339 timestamp.
const DATE_LENGTH: usize = 10;
/// `chrono` format of that prefix.
const DATE_FORMAT: &str = "%Y-%m-%d";
const LOG_EXTENSION: &str = "jsonl";
/// Appended to a finished day's file name.
const COMPRESSED_SUFFIX: &str = ".gz";
/// Appended while compressing, so a crash leaves no truncated archive.
const PARTIAL_SUFFIX: &str = ".partial";

#[derive(Args)]
pub(crate) struct CompressArgs {
    /// The daemon's `--log-dir`.
    directory: PathBuf,
}

/// `jackerymon compress-logs`: gzips each day's log once the day is over.
pub(crate) fn compress_logs(arguments: &CompressArgs) -> Result<()> {
    let today = Utc::now().format(DATE_FORMAT).to_string();
    let mut failures = 0;
    for path in finished_days(&arguments.directory, &today)? {
        if let Err(error) = compress(&path) {
            log::error!("{}: {error:#}", path.display());
            failures += 1;
        }
    }
    if failures > 0 {
        anyhow::bail!("{failures} log files left uncompressed");
    }
    Ok(())
}

/// Uncompressed logs of days before `today`, by the date in their names.
fn finished_days(directory: &Path, today: &str) -> Result<Vec<PathBuf>> {
    let mut finished = Vec::new();
    for entry in
        fs::read_dir(directory).with_context(|| format!("reading {}", directory.display()))?
    {
        let path = entry
            .with_context(|| format!("reading {}", directory.display()))?
            .path();
        let day = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|day| day.len() == DATE_LENGTH);
        // Dates in this form sort as text.
        if path
            .extension()
            .is_some_and(|extension| extension == LOG_EXTENSION)
            && day.is_some_and(|day| day < today)
        {
            finished.push(path);
        }
    }
    finished.sort();
    Ok(finished)
}

/// Writes `path` gzipped beside it, then removes the plain file.  An
/// existing archive is never replaced.
fn compress(path: &Path) -> Result<()> {
    let mut compressed_name = path.file_name().context("no file name")?.to_owned();
    compressed_name.push(COMPRESSED_SUFFIX);
    let compressed = path.with_file_name(&compressed_name);
    let mut partial_name = compressed_name;
    partial_name.push(PARTIAL_SUFFIX);
    let partial = path.with_file_name(partial_name);
    let mut input = File::open(path).context("opening")?;
    let output =
        File::create(&partial).with_context(|| format!("creating {}", partial.display()))?;
    let mut encoder = GzEncoder::new(output, Compression::default());
    io::copy(&mut input, &mut encoder).context("compressing")?;
    encoder
        .finish()
        .and_then(|file| file.sync_all())
        .context("finishing the archive")?;
    // Linking, unlike renaming, refuses to replace an archive.
    let published = fs::hard_link(&partial, &compressed)
        .with_context(|| format!("creating {}", compressed.display()));
    fs::remove_file(&partial).ok();
    published?;
    fs::remove_file(path).context("removing the plain file")
}

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
    let path = directory.join(day).with_extension(LOG_EXTENSION);
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

    use std::io::Read;

    use flate2::read::GzDecoder;

    use super::SampleLog;
    use super::compress;
    use super::finished_days;
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

    #[test]
    fn compresses_finished_days_only() {
        let directory =
            std::env::temp_dir().join(format!("jackerymon-compress-{}", std::process::id()));
        fs::create_dir_all(&directory).expect("temp dir");
        for name in [
            "2026-10-10.jsonl",
            "2026-10-11.jsonl",
            "2026-10-12.jsonl",
            "notes.jsonl",
        ] {
            fs::write(directory.join(name), format!("{name}\n")).expect("write");
        }
        let finished = finished_days(&directory, "2026-10-12").expect("list");
        let names: Vec<_> = finished
            .iter()
            .map(|path| {
                path.file_name()
                    .expect("name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["2026-10-10.jsonl", "2026-10-11.jsonl"]);
        compress(&finished[0]).expect("compress");
        let mut text = String::new();
        GzDecoder::new(fs::File::open(directory.join("2026-10-10.jsonl.gz")).expect("archive"))
            .read_to_string(&mut text)
            .expect("gunzip");
        assert_eq!(text, "2026-10-10.jsonl\n");
        assert!(!finished[0].exists(), "the plain file goes");
        // An existing archive is never replaced.
        fs::write(&finished[0], "again\n").expect("rewrite");
        assert!(compress(&finished[0]).is_err());
        assert!(
            finished[0].exists(),
            "the plain file stays when compressing fails"
        );
        fs::remove_dir_all(&directory).expect("cleanup");
    }
}
