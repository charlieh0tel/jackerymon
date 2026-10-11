//! Options shared by every subcommand that talks to the unit.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::Args;
use jackerymon::key_file::KeyFile;
use jackerymon::poll::PollConfig;

const RECONNECT_DELAY: Duration = Duration::from_secs(1);

/// Which unit to reach, and how patiently.  Global, so these may also
/// follow a `set` setting.
#[derive(Args)]
pub(crate) struct ConnectionArgs {
    /// Key file written by `tools/jackery_key.py`.
    #[arg(long, global = true, default_value = "jackery-key.json")]
    key_file: PathBuf,
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
    pub(crate) fn key_file(&self) -> Result<KeyFile> {
        let mut key_file = KeyFile::load(&self.key_file)?;
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

fn positive_seconds(text: &str) -> Result<f64, String> {
    match text.parse::<f64>() {
        Ok(seconds) if seconds.is_finite() && seconds > 0.0 => Ok(seconds),
        _ => Err("must be a positive number of seconds".to_owned()),
    }
}
