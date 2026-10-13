//! `jackerymon scan`: list the Jackery units in range, without
//! connecting or touching the cloud.

use std::time::Duration;

use anyhow::Result;
use anyhow::bail;
use clap::Args;
use jackerymon::ble;
use jackerymon::ble::Sighting;

use crate::connection::seconds;
use crate::record::UnitRecord;

/// Seconds to listen for units, unless `--timeout` says otherwise.
pub(crate) const DEFAULT_LISTEN: &str = "10";

/// Options of `jackerymon scan`.
#[derive(Args)]
pub(crate) struct ScanArgs {
    /// Seconds to listen.
    #[arg(long, default_value = DEFAULT_LISTEN, value_parser = seconds)]
    timeout: Duration,
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
pub(crate) async fn find(timeout: Duration, address: Option<&str>) -> Result<Vec<Sighting>> {
    let adapter = ble::default_adapter().await?;
    let sightings = ble::scan(&adapter, timeout, address).await?;
    if sightings.is_empty() {
        bail!("no Jackery unit heard; wake it and retry");
    }
    Ok(sightings)
}
