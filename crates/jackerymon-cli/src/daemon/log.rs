//! Daily JSON-lines sample log: one `YYYY-MM-DD.jsonl` file per UTC day.
//! `jackerymon compress-logs` gzips the finished days; the daemon itself
//! never compresses or deletes.

use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::io::BufRead;
use std::io::BufReader;
use std::io::ErrorKind;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use chrono::DateTime;
use chrono::Utc;
use clap::Args;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde::Deserialize;

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

/// Options of `jackerymon compress-logs`.
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
        bail!("{failures} log files left uncompressed");
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
/// existing archive is never replaced: if it already holds the same data,
/// left by a run that stopped before removing the plain file or by a
/// concurrent run, the plain file goes; otherwise both stay and this
/// fails.
fn compress(path: &Path) -> Result<()> {
    let mut compressed_name = path.file_name().context("no file name")?.to_owned();
    compressed_name.push(COMPRESSED_SUFFIX);
    let compressed = path.with_file_name(&compressed_name);
    if !compressed.exists() {
        // Each run writes its own temporary file, so runs cannot truncate
        // or remove each other's.
        let mut partial_name = compressed_name;
        partial_name.push(format!("{PARTIAL_SUFFIX}-{:016x}", rand::random::<u64>()));
        let partial = path.with_file_name(partial_name);
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)
            .with_context(|| format!("creating {}", partial.display()))?;
        let written = write_archive(path, &mut output).and_then(|()| {
            // Linking, unlike renaming, refuses to replace an archive.
            match fs::hard_link(&partial, &compressed) {
                Err(error) if error.kind() != ErrorKind::AlreadyExists => {
                    Err(error).with_context(|| format!("creating {}", compressed.display()))
                }
                _ => Ok(()),
            }
        });
        fs::remove_file(&partial).ok();
        written?;
    }
    if !archive_matches(&compressed, path)? {
        bail!("{} exists with other data; left both", compressed.display());
    }
    match fs::remove_file(path) {
        Err(error) if error.kind() != ErrorKind::NotFound => {
            Err(error).context("removing the plain file")
        }
        _ => Ok(()),
    }
}

/// Gzips `source` into `output`.
fn write_archive(source: &Path, output: &mut File) -> Result<()> {
    let mut input = File::open(source).context("opening")?;
    let mut encoder = GzEncoder::new(output, Compression::default());
    io::copy(&mut input, &mut encoder).context("compressing")?;
    encoder
        .finish()
        .and_then(|file| file.sync_all())
        .context("finishing the archive")
}

/// Whether `archive` decompresses to exactly `source`.  A missing source
/// counts as a match: another run already removed it.
fn archive_matches(archive: &Path, source: &Path) -> Result<bool> {
    let plain = match fs::read(source) {
        Ok(plain) => plain,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error).context("reading the plain file"),
    };
    let mut unpacked = Vec::new();
    GzDecoder::new(File::open(archive).with_context(|| format!("opening {}", archive.display()))?)
        .read_to_end(&mut unpacked)
        .with_context(|| format!("reading {}", archive.display()))?;
    Ok(unpacked == plain)
}

/// Calls `emit` with each logged sample line timestamped in
/// `since..=until`, in order, from the plain or gzipped file of each day in
/// that range.  `emit` returning false stops the read.  A line that does
/// not parse, such as one the daemon is still writing, is skipped.
pub(super) fn read_history(
    directory: &Path,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    mut emit: impl FnMut(&str) -> bool,
) -> Result<()> {
    let mut day = since.date_naive();
    while day <= until.date_naive() {
        let plain = log_path(directory, &day.format(DATE_FORMAT).to_string());
        let mut compressed_name = plain.file_name().context("no file name")?.to_owned();
        compressed_name.push(COMPRESSED_SUFFIX);
        let compressed = plain.with_file_name(compressed_name);
        // The plain file while it exists: it is complete, and an archive
        // beside it may still be being written.
        let reader: Option<Box<dyn BufRead>> = match File::open(&plain) {
            Ok(file) => Some(Box::new(BufReader::new(file))),
            Err(error) if error.kind() == ErrorKind::NotFound => match File::open(&compressed) {
                Ok(file) => Some(Box::new(BufReader::new(GzDecoder::new(file)))),
                Err(error) if error.kind() == ErrorKind::NotFound => None,
                Err(error) => {
                    return Err(error).with_context(|| format!("opening {}", compressed.display()));
                }
            },
            Err(error) => {
                return Err(error).with_context(|| format!("opening {}", plain.display()));
            }
        };
        for line in reader.into_iter().flat_map(BufRead::lines) {
            let line = line.context("reading the sample log")?;
            let Ok(stamp) = serde_json::from_str::<Stamp>(&line) else {
                continue;
            };
            let Ok(taken) = DateTime::parse_from_rfc3339(&stamp.timestamp) else {
                continue;
            };
            if (since..=until).contains(&taken.with_timezone(&Utc)) && !emit(&line) {
                return Ok(());
            }
        }
        day = day.succ_opt().context("date out of range")?;
    }
    Ok(())
}

/// The one field `read_history` needs from a sample line.
#[derive(Deserialize)]
struct Stamp {
    timestamp: String,
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

/// The plain log file for `day`, `YYYY-MM-DD`.
fn log_path(directory: &Path, day: &str) -> PathBuf {
    directory.join(day).with_extension(LOG_EXTENSION)
}

/// Opens the file for `day` to append to.
fn open(directory: &Path, day: &str) -> Result<File> {
    let path = log_path(directory, day);
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
    use super::read_history;
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
        // A run stopped after publishing leaves both; the next one finishes.
        fs::write(&finished[0], "2026-10-10.jsonl\n").expect("restore");
        compress(&finished[0]).expect("finish an interrupted run");
        assert!(!finished[0].exists());
        // An archive holding other data is never replaced.
        fs::write(&finished[0], "different\n").expect("rewrite");
        assert!(compress(&finished[0]).is_err());
        assert!(
            finished[0].exists(),
            "the plain file stays when compressing fails"
        );
        let leftovers = fs::read_dir(&directory)
            .expect("list")
            .filter(|entry| {
                entry
                    .as_ref()
                    .is_ok_and(|entry| entry.file_name().to_string_lossy().contains(".partial"))
            })
            .count();
        assert_eq!(leftovers, 0, "no temporary archive is left");
        fs::remove_dir_all(&directory).expect("cleanup");
    }

    #[test]
    fn concurrent_runs_leave_one_good_archive() {
        const RUNS: usize = 4;
        let directory =
            std::env::temp_dir().join(format!("jackerymon-concurrent-{}", std::process::id()));
        fs::create_dir_all(&directory).expect("temp dir");
        let path = directory.join("2026-10-10.jsonl");
        let text = "{\"sample\":1}\n".repeat(100_000);
        fs::write(&path, &text).expect("write");
        std::thread::scope(|scope| {
            for _ in 0..RUNS {
                // Losers may fail to read a source the winner removed.
                scope.spawn(|| compress(&path).ok());
            }
        });
        let mut unpacked = String::new();
        GzDecoder::new(fs::File::open(directory.join("2026-10-10.jsonl.gz")).expect("archive"))
            .read_to_string(&mut unpacked)
            .expect("gunzip");
        let plain_left = path.exists();
        fs::remove_dir_all(&directory).expect("cleanup");
        assert_eq!(unpacked, text);
        assert!(!plain_left);
    }

    #[test]
    fn history_spans_plain_and_gzipped_days() {
        let directory =
            std::env::temp_dir().join(format!("jackerymon-history-{}", std::process::id()));
        fs::create_dir_all(&directory).expect("temp dir");
        let line = |timestamp: &str, sample| record(timestamp, sample).to_string();
        let first_day = [
            line("2026-10-10T23:00:00.000000+00:00", 1),
            line("2026-10-10T23:59:59.000000+00:00", 2),
        ]
        .join("\n");
        let mut encoder = flate2::write::GzEncoder::new(
            fs::File::create(directory.join("2026-10-10.jsonl.gz")).expect("archive"),
            flate2::Compression::default(),
        );
        std::io::Write::write_all(&mut encoder, format!("{first_day}\n").as_bytes()).expect("gzip");
        encoder.finish().expect("finish");
        let second_day = [
            line("2026-10-11T00:00:04.000000+00:00", 3),
            "{\"timestamp\":\"half a li".to_owned(),
            line("2026-10-11T12:00:00.000000+00:00", 4),
        ]
        .join("\n");
        fs::write(
            directory.join("2026-10-11.jsonl"),
            format!("{second_day}\n"),
        )
        .expect("plain");
        let at = |text: &str| {
            chrono::DateTime::parse_from_rfc3339(text)
                .expect("time")
                .to_utc()
        };
        let mut samples = Vec::new();
        read_history(
            &directory,
            at("2026-10-10T23:30:00Z"),
            at("2026-10-11T06:00:00Z"),
            |line| {
                samples.push(
                    serde_json::from_str::<SampleRecord>(line)
                        .expect("record")
                        .sample,
                );
                true
            },
        )
        .expect("read");
        let mut first_only = 0;
        read_history(
            &directory,
            at("2026-10-10T00:00:00Z"),
            at("2026-10-12T00:00:00Z"),
            |_| {
                first_only += 1;
                false
            },
        )
        .expect("read");
        fs::remove_dir_all(&directory).expect("cleanup");
        assert_eq!(
            samples,
            [2, 3],
            "in range, across both days, partial line skipped"
        );
        assert_eq!(first_only, 1, "emit returning false stops the read");
    }
}
