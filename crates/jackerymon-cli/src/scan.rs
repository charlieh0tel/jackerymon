//! `jackerymon scan`: list the Jackery units in range, without
//! connecting or touching the cloud.

use std::time::Duration;

use anyhow::Result;
use anyhow::bail;
use clap::Args;
use jackerymon::ble;
use jackerymon::ble::Sighting;

use crate::connection::positive_seconds;
use crate::record::UnitRecord;

/// Seconds to listen for units, unless `--timeout` says otherwise.
pub(crate) const DEFAULT_LISTEN_SECONDS: f64 = 10.0;

#[derive(Args)]
pub(crate) struct ScanArgs {
    /// Seconds to listen.
    #[arg(long, default_value_t = DEFAULT_LISTEN_SECONDS, value_parser = positive_seconds)]
    timeout: f64,
    /// Only this BLE address.
    #[arg(long)]
    address: Option<String>,
}

pub(crate) async fn run(arguments: ScanArgs) -> Result<()> {
    let sightings = find(arguments.timeout, arguments.address.as_deref()).await?;
    for sighting in &sightings {
        println!("{}", serde_json::to_string(&UnitRecord::from(sighting))?);
    }
    Ok(())
}

/// The units heard within `timeout` seconds, optionally only `address`;
/// fails if there are none.
pub(crate) async fn find(timeout: f64, address: Option<&str>) -> Result<Vec<Sighting>> {
    let adapter = ble::default_adapter().await?;
    let sightings: Vec<Sighting> = ble::scan(&adapter, Duration::from_secs_f64(timeout))
        .await?
        .into_iter()
        .filter(|sighting| {
            address.is_none_or(|address| sighting.address.eq_ignore_ascii_case(address))
        })
        .collect();
    if sightings.is_empty() {
        bail!("no Jackery unit heard; wake it and retry");
    }
    Ok(sightings)
}
