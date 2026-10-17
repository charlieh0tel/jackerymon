//! Options shared by every subcommand that talks to the unit.

use std::env;
use std::fs;
use std::num::NonZeroU32;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use clap::Args;
use jackerymon::key_file::KeyFile;
use jackerymon::poll::PollConfig;

use crate::names::Names;
use crate::runtime;
use crate::runtime::UnitLock;

const RECONNECT_DELAY: Duration = Duration::from_secs(1);
/// Seconds allowed for discovery, connection, and each query.
const DEFAULT_TIMEOUT: &str = "10";
/// Seconds between a response and the next request.
const DEFAULT_INTERVAL: &str = "5";
/// The daemon's key directory.
const SYSTEM_KEY_DIRECTORY: &str = "/etc/jackerymon";
/// Under `$XDG_CONFIG_HOME`, or `~/.config` without it.
const USER_KEY_SUBDIRECTORY: &str = "jackerymon";
const USER_CONFIG_FALLBACK: &str = ".config";
const KEY_EXTENSION: &str = "json";

/// Where keys live when `--key-file` is not given: one `<serial>.json` per
/// unit.
#[derive(Clone, Copy)]
pub(crate) enum KeyHome {
    /// `/etc/jackerymon`, for the daemon.
    System,
    /// `~/.config/jackerymon`, for everything else.
    User,
}

impl KeyHome {
    pub(crate) fn directory(self) -> Result<PathBuf> {
        match self {
            Self::System => Ok(PathBuf::from(SYSTEM_KEY_DIRECTORY)),
            Self::User => {
                let config = match env::var_os("XDG_CONFIG_HOME") {
                    Some(config) if !config.is_empty() => PathBuf::from(config),
                    _ => PathBuf::from(env::var_os("HOME").context("HOME is not set")?)
                        .join(USER_CONFIG_FALLBACK),
                };
                Ok(config.join(USER_KEY_SUBDIRECTORY))
            }
        }
    }
}

/// The key file for `serial` in `directory`.
pub(crate) fn key_path(directory: &Path, serial: &str) -> PathBuf {
    directory.join(serial).with_extension(KEY_EXTENSION)
}

/// Every key file in `directory`, sorted.
fn keys_in(directory: &Path) -> Result<Vec<PathBuf>> {
    let entries = fs::read_dir(directory)
        .with_context(|| format!("no keys in {}; run `jackerymon key`", directory.display()))?;
    let mut keys = Vec::new();
    for entry in entries {
        let path = entry
            .with_context(|| format!("reading {}", directory.display()))?
            .path();
        if path
            .extension()
            .is_some_and(|extension| extension == KEY_EXTENSION)
        {
            keys.push(path);
        }
    }
    keys.sort();
    Ok(keys)
}

/// Every key in `directory`; one that does not load, or repeats a unit,
/// is reported and left out.
fn load_all(directory: &Path) -> Result<Vec<KeyFile>> {
    let mut loaded: Vec<KeyFile> = Vec::new();
    for path in keys_in(directory)? {
        match KeyFile::load(&path) {
            Ok(key_file) if loaded.iter().any(|other| other.serial == key_file.serial) => {
                log::error!(
                    "{}: unit {} again; left out",
                    path.display(),
                    key_file.serial
                );
            }
            Ok(key_file) => loaded.push(key_file),
            Err(error) => log::error!("{}: {error:#}; left out", path.display()),
        }
    }
    if loaded.is_empty() {
        bail!("no usable keys in {}", directory.display());
    }
    Ok(loaded)
}

/// The key file in `directory` for `unit`, or the only one there.
fn find_key(directory: &Path, unit: Option<&str>) -> Result<PathBuf> {
    if let Some(unit) = unit {
        return Ok(key_path(directory, unit));
    }
    let keys = keys_in(directory)?;
    match keys.as_slice() {
        [] => bail!("no keys in {}; run `jackerymon key`", directory.display()),
        [only] => Ok(only.clone()),
        several => {
            let serials: Vec<_> = several
                .iter()
                .filter_map(|path| path.file_stem()?.to_str())
                .collect();
            bail!(
                "several keys in {} ({}); choose one with --unit",
                directory.display(),
                serials.join(", ")
            )
        }
    }
}

/// Which unit to reach, and how patiently.  Global, so these may also
/// follow a `set` setting.
#[derive(Args)]
pub(crate) struct ConnectionArgs {
    /// Unit to reach, by serial or by its name in the key directory's
    /// `names` file: its key is `<serial>.json` there.  Optional when that
    /// directory holds one key.
    #[arg(long, global = true, value_name = "UNIT")]
    unit: Option<String>,
    /// Key file to use instead of the key directory.
    #[arg(long, global = true, conflicts_with = "unit")]
    key_file: Option<PathBuf>,
    /// BLE address, overriding the key file's.
    #[arg(long, global = true)]
    address: Option<String>,
    /// Seconds allowed for discovery, connection, and each query.
    #[arg(long, global = true, default_value = DEFAULT_TIMEOUT, value_parser = seconds)]
    timeout: Duration,
    /// Seconds between a response and the next request.
    #[arg(long, global = true, default_value = DEFAULT_INTERVAL, value_parser = seconds)]
    interval: Duration,
}

impl ConnectionArgs {
    /// The unit named by `--unit` or `--key-file`, if either is given.
    pub(crate) fn named_unit(&self) -> Result<Option<String>> {
        if let Some(serial) = self.unit_serial(KeyHome::User)? {
            return Ok(Some(serial));
        }
        match &self.key_file {
            Some(path) => Ok(Some(KeyFile::load(path)?.serial.to_string())),
            None => Ok(None),
        }
    }

    /// The chosen unit's key, with this process's lock on the unit, which
    /// must be held while using its BLE connection.
    pub(crate) fn claim(&self, home: KeyHome) -> Result<(KeyFile, UnitLock)> {
        let key_file = self.key_file(home)?;
        let lock = runtime::lock_unit(key_file.serial.as_str())?;
        Ok((key_file, lock))
    }

    /// The keys of the units a daemon serves: the one named by `--unit`
    /// or `--key-file`, or else every key in `home`.  A key that does not
    /// load, or repeats a unit, is reported and left out.
    pub(crate) fn key_files(&self, home: KeyHome) -> Result<Vec<KeyFile>> {
        if self.unit.is_some() || self.key_file.is_some() {
            return Ok(vec![self.key_file(home)?]);
        }
        if self.address.is_some() {
            bail!("--address is for one unit; choose it with --unit");
        }
        load_all(&home.directory()?)
    }

    /// `--unit` as a serial: a name from the `names` file in `home`
    /// stands for its unit's serial.
    fn unit_serial(&self, home: KeyHome) -> Result<Option<String>> {
        let Some(unit) = &self.unit else {
            return Ok(None);
        };
        let names = Names::load(&home.directory()?)?;
        Ok(Some(
            names
                .serial(unit)
                .map_or_else(|| unit.clone(), ToOwned::to_owned),
        ))
    }

    /// The key for the chosen unit, looked up in `home` unless
    /// `--key-file` names one.
    fn key_file(&self, home: KeyHome) -> Result<KeyFile> {
        let unit = self.unit_serial(home)?;
        let path = match &self.key_file {
            Some(path) => path.clone(),
            None => find_key(&home.directory()?, unit.as_deref())?,
        };
        let mut key_file = KeyFile::load(&path)?;
        // `--unit A` picks `A.json`; a copied or renamed file could hold
        // another unit's key.
        if let Some(unit) = &unit
            && key_file.serial.as_str() != unit
        {
            bail!(
                "{} holds the key for unit {}, not {unit}",
                path.display(),
                key_file.serial
            );
        }
        if let Some(address) = &self.address {
            key_file.address.clone_from(address);
        }
        Ok(key_file)
    }

    pub(crate) fn poll_config(&self, max_consecutive_failures: Option<NonZeroU32>) -> PollConfig {
        PollConfig {
            timeout: self.timeout,
            interval: self.interval,
            reconnect_delay: RECONNECT_DELAY,
            max_consecutive_failures,
        }
    }
}

/// A positive number of seconds that fits a `Duration` and does not round
/// to zero.
pub(crate) fn seconds(text: &str) -> Result<Duration, String> {
    let invalid = || "must be a positive number of seconds".to_owned();
    let seconds: f64 = text.parse().map_err(|_| invalid())?;
    match Duration::try_from_secs_f64(seconds) {
        Ok(duration) if !duration.is_zero() => Ok(duration),
        _ => Err(invalid()),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::find_key;
    use super::key_path;
    use super::load_all;
    use super::seconds;

    #[test]
    fn finds_the_only_key_or_asks_for_a_unit() {
        let directory =
            std::env::temp_dir().join(format!("jackerymon-keys-{}", std::process::id()));
        fs::create_dir_all(&directory).expect("temp dir");
        assert!(find_key(&directory, None).is_err(), "empty");
        fs::write(key_path(&directory, "111"), "{}").expect("write");
        fs::write(directory.join("notes.txt"), "").expect("write");
        assert_eq!(
            find_key(&directory, None).expect("only"),
            key_path(&directory, "111")
        );
        fs::write(key_path(&directory, "222"), "{}").expect("write");
        let several = find_key(&directory, None).expect_err("several").to_string();
        assert_eq!(
            find_key(&directory, Some("222")).expect("chosen"),
            key_path(&directory, "222")
        );
        fs::remove_dir_all(&directory).expect("cleanup");
        assert!(several.contains("111, 222"), "{several}");
    }

    #[test]
    fn loads_every_usable_key() {
        let directory =
            std::env::temp_dir().join(format!("jackerymon-all-keys-{}", std::process::id()));
        fs::create_dir_all(&directory).expect("temp dir");
        assert!(load_all(&directory).is_err(), "empty");
        let key = |serial: &str| {
            serde_json::json!({
                "address": "90:E5:B1:21:ED:BA",
                "serial": serial,
                "model": 12,
                "bluetoothKey": "dGVzdC1rZXk=",
            })
            .to_string()
        };
        fs::write(key_path(&directory, "123456789"), key("123456789")).expect("write");
        fs::write(key_path(&directory, "987654321"), key("987654321")).expect("write");
        fs::write(key_path(&directory, "copy"), key("123456789")).expect("write");
        fs::write(key_path(&directory, "broken"), "{").expect("write");
        let loaded = load_all(&directory).expect("loaded");
        fs::remove_dir_all(&directory).expect("cleanup");
        let serials: Vec<_> = loaded.iter().map(|key| key.serial.to_string()).collect();
        assert_eq!(serials, ["123456789", "987654321"]);
    }

    #[test]
    fn seconds_must_be_positive_and_representable() {
        assert_eq!(seconds("2.5"), Ok(std::time::Duration::from_millis(2500)));
        for bad in ["0", "-1", "nan", "inf", "1e100", "1e-12", "ten"] {
            assert!(seconds(bad).is_err(), "{bad}");
        }
    }
}
