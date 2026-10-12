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

use crate::runtime;
use crate::runtime::UnitLock;

const RECONNECT_DELAY: Duration = Duration::from_secs(1);
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

/// The key file in `directory` for `unit`, or the only one there.
fn find_key(directory: &Path, unit: Option<&str>) -> Result<PathBuf> {
    if let Some(unit) = unit {
        return Ok(key_path(directory, unit));
    }
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
    /// Unit to reach, by serial: its key is `<serial>.json` in the key
    /// directory.  Optional when that directory holds one key.
    #[arg(long, global = true, value_name = "SERIAL")]
    unit: Option<String>,
    /// Key file to use instead of the key directory.
    #[arg(long, global = true, conflicts_with = "unit")]
    key_file: Option<PathBuf>,
    /// BLE address, overriding the key file's.
    #[arg(long, global = true)]
    address: Option<String>,
    /// Seconds allowed for discovery, connection, and each query.
    #[arg(long, global = true, default_value_t = 10.0, value_parser = positive_seconds)]
    timeout: f64,
    /// Seconds between a response and the next request.
    #[arg(long, global = true, default_value_t = 5.0, value_parser = positive_seconds)]
    interval: f64,
}

impl ConnectionArgs {
    /// The unit named by `--unit` or `--key-file`, if either is given.
    pub(crate) fn named_unit(&self) -> Result<Option<String>> {
        if let Some(unit) = &self.unit {
            return Ok(Some(unit.clone()));
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

    /// The key for the chosen unit, looked up in `home` unless
    /// `--key-file` names one.
    fn key_file(&self, home: KeyHome) -> Result<KeyFile> {
        let path = match &self.key_file {
            Some(path) => path.clone(),
            None => find_key(&home.directory()?, self.unit.as_deref())?,
        };
        let mut key_file = KeyFile::load(&path)?;
        if let Some(address) = &self.address {
            key_file.address.clone_from(address);
        }
        Ok(key_file)
    }

    pub(crate) fn poll_config(&self, max_consecutive_failures: Option<NonZeroU32>) -> PollConfig {
        PollConfig {
            timeout: Duration::from_secs_f64(self.timeout),
            interval: Duration::from_secs_f64(self.interval),
            reconnect_delay: RECONNECT_DELAY,
            max_consecutive_failures,
        }
    }
}

pub(crate) fn positive_seconds(text: &str) -> Result<f64, String> {
    match text.parse::<f64>() {
        Ok(seconds) if seconds.is_finite() && seconds > 0.0 => Ok(seconds),
        _ => Err("must be a positive number of seconds".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::find_key;
    use super::key_path;

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
}
