//! BLE transport via bluer (BlueZ): find the saved unit, connect, and
//! query.

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::future::Future;
use std::num::NonZeroU8;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::time::Duration;

use bluer::Adapter;
use bluer::AdapterEvent;
use bluer::Address;
use bluer::AddressType;
use bluer::Device;
use bluer::DeviceEvent;
use bluer::DeviceProperty;
use bluer::DiscoveryFilter;
use bluer::DiscoveryTransport;
use bluer::Session;
use bluer::gatt::WriteOp;
use bluer::gatt::remote::Characteristic;
use bluer::gatt::remote::CharacteristicWriteRequest;
use futures::Stream;
use futures::StreamExt;
use log::debug;
use log::info;
use log::warn;
use thiserror::Error;
use tokio::sync::Mutex;
use tokio::time::timeout;
use uuid::Uuid;

use crate::advertisement;
use crate::advertisement::Advertisement;
use crate::frame::DecodeError;
use crate::frame::RawStatus;
use crate::frame::Request;
use crate::frame::ResponseDecoder;
use crate::identity::BleKey;
use crate::identity::ModelCode;
use crate::key_file::KeyFile;
use crate::poll::Commands;
use crate::poll::Connector;
use crate::poll::Link;
use crate::poll::PollConfig;
use crate::poll::PollError;
use crate::poll::PollEvent;
use crate::poll::poll_with;

/// Jackery service; its service data carries the advertisement payload.
const SERVICE: Uuid = bluetooth_uuid(0xbdee);
/// Bound on BlueZ calls outside a connect attempt, such as finding the
/// adapter; bluer's own D-Bus timeout is two minutes.
const SETUP_TIMEOUT: Duration = Duration::from_secs(10);
/// Searches in a row that miss the unit before its BlueZ object is
/// removed; see [`Unit::clear_stale`].
const STALE_AFTER_MISSES: u32 = 3;
/// Held while a unit is found and connected.  BlueZ makes one LE
/// connection at a time per adapter and fails a second attempt with
/// "Operation already in progress", so units polled by one process take
/// turns.
static CONNECTING: Mutex<()> = Mutex::const_new(());
/// Write-without-response command characteristic.
const COMMAND: Uuid = bluetooth_uuid(0xee01);
/// Notify characteristic carrying responses.
const NOTIFY: Uuid = bluetooth_uuid(0xee02);

/// The Bluetooth base UUID with a 16-bit short UUID in bits 96..112.
const fn bluetooth_uuid(short: u16) -> Uuid {
    const BASE: u128 = 0x0000_0000_0000_1000_8000_0080_5f9b_34fb;
    const SHORT_SHIFT: u32 = 96;
    Uuid::from_u128(BASE | ((short as u128) << SHORT_SHIFT))
}

type Notifications = Pin<Box<dyn Stream<Item = Vec<u8>> + Send>>;
type DeviceEvents = Pin<Box<dyn Stream<Item = DeviceEvent> + Send>>;

/// Why a BLE operation failed.
#[derive(Debug, Error)]
pub enum BleError {
    #[error(transparent)]
    Bluez(#[from] bluer::Error),
    #[error("invalid BLE address {0:?} in the key file")]
    InvalidAddress(String),
    #[error("saved device identity not found in BLE advertisements")]
    NotFound,
    #[error("timed out {0}")]
    TimedOut(&'static str),
    #[error("GATT characteristic {0} not found")]
    MissingCharacteristic(Uuid),
    #[error("device disconnected")]
    Disconnected,
    #[error("no complete status response ({received} notification bytes)")]
    NoResponse { received: usize },
    #[error(transparent)]
    Decode(#[from] DecodeError),
}

impl BleError {
    /// Whether reconnecting might help.  Invalid data means a wrong key or
    /// codec, which a retry cannot fix.
    pub(crate) fn is_transport(&self) -> bool {
        !matches!(self, Self::Decode(_) | Self::InvalidAddress(_))
    }
}

/// The default Bluetooth adapter.
///
/// # Errors
///
/// Fails if BlueZ is unreachable or there is no adapter.
pub async fn default_adapter() -> Result<Adapter, BleError> {
    let session = bounded(SETUP_TIMEOUT, "connecting to BlueZ", Session::new()).await?;
    bounded(
        SETUP_TIMEOUT,
        "finding the Bluetooth adapter",
        session.default_adapter(),
    )
    .await
}

/// Polls the unit named by `key_file`; see [`crate::poll`].
///
/// # Errors
///
/// Fails on invalid protocol data, or when the failure limit in `config`
/// is reached.
pub async fn poll(
    adapter: Adapter,
    key_file: KeyFile,
    config: PollConfig,
    commands: Commands,
    stop: impl Future<Output = ()>,
    on_event: impl FnMut(PollEvent) -> ControlFlow<()>,
) -> Result<(), PollError> {
    let address = key_file
        .address
        .parse()
        .map_err(|_| PollError::Fatal(BleError::InvalidAddress(key_file.address.clone())))?;
    let unit = Unit {
        adapter,
        address,
        key_file,
        misses: 0,
    };
    poll_with(unit, config, commands, stop, on_event).await
}

/// A Jackery unit heard during [`scan`].
#[derive(Clone, Debug)]
pub struct Sighting {
    /// BLE address, as `XX:XX:XX:XX:XX:XX`.
    pub address: String,
    pub advertisement: Advertisement,
}

/// Listens for `duration` and returns every Jackery power station heard,
/// or only the one at `only`, one entry per address.  No connection is
/// made.
///
/// # Errors
///
/// Fails if BlueZ cannot scan or `only` is not an address.
pub async fn scan(
    adapter: &Adapter,
    duration: Duration,
    only: Option<&str>,
) -> Result<Vec<Sighting>, BleError> {
    let only: Option<Address> = only
        .map(|text| {
            text.parse()
                .map_err(|_| BleError::InvalidAddress(text.to_owned()))
        })
        .transpose()?;
    let filter = adapter.set_discovery_filter(DiscoveryFilter {
        transport: DiscoveryTransport::Le,
        duplicate_data: true,
        uuids: HashSet::from([SERVICE]),
        pattern: only.map(|address| address.to_string()),
        ..DiscoveryFilter::default()
    });
    bounded(SETUP_TIMEOUT, "setting the scan filter", filter).await?;
    let mut heard_by_address = BTreeMap::new();
    // Discovery stops when this stream is dropped.
    let mut discovery = Box::pin(
        bounded(
            SETUP_TIMEOUT,
            "starting the scan",
            adapter.discover_devices_with_changes(),
        )
        .await?,
    );
    let listening = async {
        while let Some(event) = discovery.next().await {
            let AdapterEvent::DeviceAdded(address) = event else {
                continue;
            };
            if only.is_some_and(|only| only != address) {
                continue;
            }
            // A device can vanish between being listed and being read;
            // that ends its entry, not the scan.
            match heard(&adapter.device(address)?).await {
                Ok(Some(advertisement)) => {
                    heard_by_address.insert(address, advertisement);
                }
                Ok(None) => {}
                Err(error) => debug!("skipping {address}: {error}"),
            }
        }
        Ok::<(), BleError>(())
    };
    // Listening ends at the deadline; an early end is an error or silence.
    if let Ok(result) = timeout(duration, listening).await {
        result?;
    }
    Ok(heard_by_address
        .into_iter()
        .map(|(address, advertisement)| Sighting {
            address: address.to_string(),
            advertisement,
        })
        .collect())
}

/// The saved unit, reachable through one adapter.
struct Unit {
    adapter: Adapter,
    /// The key file's address, parsed.
    address: Address,
    key_file: KeyFile,
    /// Searches in a row that did not find the unit.
    misses: u32,
}

impl Unit {
    /// Removes a random-type object holding the unit's address.  The unit
    /// also sends empty random-type advertisements from its public
    /// address; BlueZ keeps one object per address, so if one of those
    /// registers first, every real advertisement is refused until the
    /// object goes (see `FINDINGS.md`, Duplicate advertisement).
    ///
    /// Only an object matching that signature goes: random type, not
    /// paired or trusted, and carrying no advertisement data, so a wrong
    /// `--address` cannot delete another device's pairing.
    async fn clear_impostor(&self, limit: Duration) -> Result<(), BleError> {
        let device = self.adapter.device(self.address)?;
        // An error here means there is no object, which is fine.
        if !bounded(limit, "inspecting the unit's object", is_impostor(&device))
            .await
            .unwrap_or(false)
        {
            return Ok(());
        }
        bounded(
            limit,
            "removing the stale object",
            self.adapter.remove_device(self.address),
        )
        .await?;
        info!(
            "removed BlueZ's random-type object for {}, left by the unit's empty duplicate advertisement",
            self.address
        );
        Ok(())
    }

    /// Removes BlueZ's object for the unit, so its next advertisement
    /// makes a fresh one.  After a connect failed, the object stayed
    /// without matching while the unit advertised, and BlueZ never dropped
    /// it, since scans kept coming (2026-10-16).  Only an object that is
    /// not connected, paired or trusted goes; what it held is logged.
    async fn clear_stale(&mut self, limit: Duration) -> Result<(), BleError> {
        let device = self.adapter.device(self.address)?;
        // An error here means there is no object, which is fine.
        let Ok(Some(state)) =
            bounded(limit, "inspecting the unit's object", stale_state(&device)).await
        else {
            return Ok(());
        };
        bounded(
            limit,
            "removing the stale object",
            self.adapter.remove_device(self.address),
        )
        .await?;
        self.misses = 0;
        info!(
            "removed BlueZ's object for {} after {STALE_AFTER_MISSES} searches missed the unit; it held {state}",
            self.address
        );
        Ok(())
    }

    /// Waits for an advertisement, seen during this scan, whose serial and
    /// model match the key file.
    async fn find(
        &self,
        discovery: &mut (impl Stream<Item = AdapterEvent> + Unpin),
    ) -> Result<Device, BleError> {
        while let Some(event) = discovery.next().await {
            if let AdapterEvent::DeviceAdded(address) = event
                && address == self.address
            {
                let device = self.adapter.device(address)?;
                if self.matches(&device).await? {
                    return Ok(device);
                }
            }
        }
        Err(BleError::NotFound)
    }

    async fn matches(&self, device: &Device) -> Result<bool, BleError> {
        Ok(heard(device).await?.is_some_and(|decoded| {
            decoded.serial == self.key_file.serial && decoded.model == self.key_file.model
        }))
    }

    /// Connects, resolves services, and subscribes to notifications.
    async fn open(&self, device: &Device, limit: Duration) -> Result<Connection, BleError> {
        // Subscribe before connecting so no disconnect is missed.
        let events = Box::pin(bounded(limit, "watching the device", device.events()).await?);
        bounded(limit, "connecting", device.connect()).await?;
        let services = bounded(limit, "discovering services", device.services()).await?;
        let (command, notify) = bounded(limit, "discovering characteristics", async {
            let mut command = None;
            let mut notify = None;
            for service in services {
                for characteristic in service.characteristics().await? {
                    match characteristic.uuid().await? {
                        COMMAND => command = Some(characteristic),
                        NOTIFY => notify = Some(characteristic),
                        _ => {}
                    }
                }
            }
            Ok((command, notify))
        })
        .await?;
        let command = command.ok_or(BleError::MissingCharacteristic(COMMAND))?;
        let notify = notify.ok_or(BleError::MissingCharacteristic(NOTIFY))?;
        let notifications = Box::pin(bounded(limit, "subscribing", notify.notify()).await?);
        Ok(Connection {
            device: device.clone(),
            command,
            notifications,
            events,
            key: self.key_file.key.clone(),
            limit,
        })
    }

    async fn open_or_disconnect(
        &self,
        device: &Device,
        limit: Duration,
    ) -> Result<Connection, BleError> {
        match self.open(device, limit).await {
            Ok(connection) => Ok(connection),
            Err(error) => {
                // The setup error matters more than a failed cleanup.
                cancel(device, limit).await;
                Err(error)
            }
        }
    }
}

impl Connector for Unit {
    type Link = Connection;

    fn model(&self) -> ModelCode {
        self.key_file.model
    }

    async fn connect(&mut self, limit: Duration) -> Result<Connection, BleError> {
        let _turn = CONNECTING.lock().await;
        // A process that exited without disconnecting leaves BlueZ
        // connected, and a connected unit does not advertise.
        let device = self.adapter.device(self.address)?;
        if is_connected(&device, limit).await {
            info!("reusing the open BlueZ connection to {}", self.address);
            return self.open_or_disconnect(&device, limit).await;
        }
        self.clear_impostor(limit).await?;
        if self.misses >= STALE_AFTER_MISSES {
            self.clear_stale(limit).await?;
        }
        // Only objects for addresses starting with the unit's are created,
        // which keeps other devices' advertisements out of BlueZ.
        let filter = self.adapter.set_discovery_filter(DiscoveryFilter {
            transport: DiscoveryTransport::Le,
            duplicate_data: true,
            pattern: Some(self.address.to_string()),
            ..DiscoveryFilter::default()
        });
        bounded(limit, "setting the scan filter", filter).await?;
        let found = {
            // Discovery stops when this stream is dropped.
            let mut discovery = Box::pin(
                bounded(
                    limit,
                    "starting the scan",
                    self.adapter.discover_devices_with_changes(),
                )
                .await?,
            );
            timeout(limit, self.find(&mut discovery)).await
        };
        let device = match found.unwrap_or(Err(BleError::NotFound)) {
            Ok(device) => device,
            Err(error) => {
                if matches!(error, BleError::NotFound) {
                    self.misses += 1;
                }
                return Err(error);
            }
        };
        self.misses = 0;
        self.open_or_disconnect(&device, limit).await
    }

    async fn abandon(&mut self, limit: Duration) {
        if let Ok(device) = self.adapter.device(self.address)
            && is_connected(&device, limit).await
        {
            disconnect(&device, limit).await;
        }
    }
}

/// One connection with its notification subscription.
struct Connection {
    device: Device,
    command: Characteristic,
    notifications: Notifications,
    /// The device's property changes, watched for a disconnect.
    events: DeviceEvents,
    key: BleKey,
    /// Bound on the disconnect at close.
    limit: Duration,
}

impl Link for Connection {
    async fn exchange(
        &mut self,
        request: &Request,
        limit: Duration,
    ) -> Result<RawStatus, BleError> {
        let mut decoder = ResponseDecoder::new(self.key.clone(), request.action);
        let mask = NonZeroU8::new(rand::random_range(1..=u8::MAX)).expect("mask is nonzero");
        let write = CharacteristicWriteRequest {
            op_type: WriteOp::Command,
            ..CharacteristicWriteRequest::default()
        };
        bounded(
            limit,
            "writing the request",
            self.command
                .write_ext(&request.encode(&self.key, mask), &write),
        )
        .await?;
        let outcome = timeout(limit, async {
            loop {
                tokio::select! {
                    notification = self.notifications.next() => {
                        let notification = notification.ok_or(BleError::Disconnected)?;
                        if let Some(status) = decoder.feed(&notification)? {
                            return Ok(status);
                        }
                    }
                    event = self.events.next() => {
                        if is_disconnect(event.as_ref()) {
                            return Err(BleError::Disconnected);
                        }
                    }
                }
            }
        })
        .await;
        outcome.unwrap_or_else(|_| {
            Err(BleError::NoResponse {
                received: decoder.received(),
            })
        })
    }

    async fn idle(&mut self, duration: Duration) -> Result<(), BleError> {
        // Notifications outside a request are discarded; the wire has no
        // sequence number to match them to one.
        let watched = timeout(duration, async {
            loop {
                tokio::select! {
                    notification = self.notifications.next() => {
                        if notification.is_none() {
                            return BleError::Disconnected;
                        }
                    }
                    event = self.events.next() => {
                        if is_disconnect(event.as_ref()) {
                            return BleError::Disconnected;
                        }
                    }
                }
            }
        })
        .await;
        watched.map_or(Ok(()), Err)
    }

    async fn close(self) {
        // Disconnect stops notifications, so there is no unsubscribe.
        disconnect(&self.device, self.limit).await;
    }
}

/// The Jackery advertisement `device` sent during the current scan, if
/// any.
async fn heard(device: &Device) -> Result<Option<Advertisement>, BleError> {
    // BlueZ clears RSSI when a scan ends, so a value means the device was
    // heard during this one, not just remembered.
    if device.rssi().await?.is_none() {
        return Ok(None);
    }
    let Some(service_data) = device.service_data().await? else {
        return Ok(None);
    };
    let Some(service_data) = service_data.get(&SERVICE) else {
        return Ok(None);
    };
    let manufacturer = device.manufacturer_data().await?.unwrap_or_default();
    let manufacturer = manufacturer
        .iter()
        .map(|(company, value)| (*company, value.as_slice()));
    Ok(advertisement::decode(manufacturer, service_data))
}

/// Whether `device` looks like the unit's empty random-type duplicate.
async fn is_impostor(device: &Device) -> bluer::Result<bool> {
    Ok(device.address_type().await? == AddressType::LeRandom
        && !device.is_paired().await?
        && !device.is_trusted().await?
        && device
            .service_data()
            .await?
            .is_none_or(|data| data.is_empty())
        && device
            .manufacturer_data()
            .await?
            .is_none_or(|data| data.is_empty()))
}

/// What a disconnected, unpaired, untrusted object holds, for the log;
/// `None` for any other.
async fn stale_state(device: &Device) -> bluer::Result<Option<String>> {
    if device.is_connected().await? || device.is_paired().await? || device.is_trusted().await? {
        return Ok(None);
    }
    let present = |has: bool| if has { "present" } else { "none" };
    let rssi = device
        .rssi()
        .await?
        .map_or_else(|| "none".to_owned(), |rssi| format!("{rssi} dBm"));
    let service = device
        .service_data()
        .await?
        .is_some_and(|data| !data.is_empty());
    let manufacturer = device
        .manufacturer_data()
        .await?
        .is_some_and(|data| !data.is_empty());
    Ok(Some(format!(
        "a {:?} address, RSSI {rssi}, service data {}, manufacturer data {}",
        device.address_type().await?,
        present(service),
        present(manufacturer)
    )))
}

/// Whether BlueZ shows `device` connected; an unanswered or failed query
/// counts as no.
async fn is_connected(device: &Device, limit: Duration) -> bool {
    bounded(limit, "checking the connection", device.is_connected())
        .await
        .unwrap_or(false)
}

/// Disconnects within `limit`.  A device already gone is fine; any other
/// failure is logged, since the unit may stay connected and unreachable.
async fn disconnect(device: &Device, limit: Duration) {
    if !is_connected(device, limit).await {
        return;
    }
    if let Err(error) = bounded(limit, "disconnecting", device.disconnect()).await {
        warn!("disconnecting {}: {error}", device.address());
    }
}

/// Ends a connection attempt that failed.  BlueZ goes on with a
/// `Connect` the caller stopped waiting for, and while it does, refuses
/// every other connection on the adapter with "Operation already in
/// progress"; a disconnect cancels it.  The device is not connected, so
/// [`disconnect`], which checks first, would skip it.  Nothing to end is
/// the usual failure, so failures are only logged at debug level.
async fn cancel(device: &Device, limit: Duration) {
    if let Err(error) = bounded(limit, "cancelling the connection", device.disconnect()).await {
        debug!("cancelling the connection to {}: {error}", device.address());
    }
}

/// A disconnect, or the end of the device's events (it was removed).
fn is_disconnect(event: Option<&DeviceEvent>) -> bool {
    matches!(
        event,
        None | Some(DeviceEvent::PropertyChanged(DeviceProperty::Connected(
            false
        )))
    )
}

async fn bounded<T>(
    limit: Duration,
    action: &'static str,
    operation: impl Future<Output = bluer::Result<T>>,
) -> Result<T, BleError> {
    Ok(timeout(limit, operation)
        .await
        .map_err(|_| BleError::TimedOut(action))??)
}

#[cfg(test)]
mod tests {
    use super::SERVICE;

    #[test]
    fn short_uuids_expand_on_the_bluetooth_base() {
        assert_eq!(SERVICE.to_string(), "0000bdee-0000-1000-8000-00805f9b34fb");
    }
}
