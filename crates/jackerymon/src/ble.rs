//! BLE transport via btleplug: find the saved unit, connect, and query.
//!
//! Kept small so a move to `bluer` stays cheap (see `PLAN.md`).

use std::future::Future;
use std::num::NonZeroU8;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::time::Duration;

use btleplug::api::Central;
use btleplug::api::CentralEvent;
use btleplug::api::Characteristic;
use btleplug::api::Manager as _;
use btleplug::api::Peripheral as _;
use btleplug::api::ScanFilter;
use btleplug::api::ValueNotification;
use btleplug::api::WriteType;
use btleplug::api::bleuuid::uuid_from_u16;
use btleplug::platform::Adapter;
use btleplug::platform::Manager;
use btleplug::platform::Peripheral;
use btleplug::platform::PeripheralId;
use futures::Stream;
use futures::StreamExt;
use thiserror::Error;
use tokio::time::timeout;
use uuid::Uuid;

use crate::advertisement;
use crate::frame::DecodeError;
use crate::frame::RawStatus;
use crate::frame::Request;
use crate::frame::ResponseDecoder;
use crate::identity::BleKey;
use crate::key_file::KeyFile;
use crate::poll::Commands;
use crate::poll::Connector;
use crate::poll::Link;
use crate::poll::PollConfig;
use crate::poll::PollError;
use crate::poll::PollEvent;
use crate::poll::poll_with;

/// Jackery service; its service data carries the advertisement payload.
const SERVICE: Uuid = uuid_from_u16(0xbdee);
/// Write-without-response command characteristic.
const COMMAND: Uuid = uuid_from_u16(0xee01);
/// Notify characteristic carrying responses.
const NOTIFY: Uuid = uuid_from_u16(0xee02);

type Events = Pin<Box<dyn Stream<Item = CentralEvent> + Send>>;
type Notifications = Pin<Box<dyn Stream<Item = ValueNotification> + Send>>;

#[derive(Debug, Error)]
pub enum BleError {
    #[error(transparent)]
    Btleplug(#[from] btleplug::Error),
    #[error("no Bluetooth adapter available")]
    NoAdapter,
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
        !matches!(self, Self::Decode(_) | Self::NoAdapter)
    }
}

/// The first Bluetooth adapter.
///
/// # Errors
///
/// Fails if BlueZ is unreachable or there is no adapter.
pub async fn default_adapter() -> Result<Adapter, BleError> {
    Manager::new()
        .await?
        .adapters()
        .await?
        .into_iter()
        .next()
        .ok_or(BleError::NoAdapter)
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
    poll_with(Unit { adapter, key_file }, config, commands, stop, on_event).await
}

/// The saved unit, reachable through one adapter.
struct Unit {
    adapter: Adapter,
    key_file: KeyFile,
}

impl Unit {
    /// Waits for a fresh advertisement whose address, serial, and model
    /// match the key file.
    async fn find(&self, events: &mut Events) -> Result<Peripheral, BleError> {
        loop {
            let id = match events.next().await {
                Some(
                    CentralEvent::DeviceDiscovered(id)
                    | CentralEvent::DeviceUpdated(id)
                    | CentralEvent::ManufacturerDataAdvertisement { id, .. }
                    | CentralEvent::ServiceDataAdvertisement { id, .. },
                ) => id,
                Some(_) => continue,
                None => return Err(BleError::NotFound),
            };
            let peripheral = self.adapter.peripheral(&id).await?;
            if self.matches(&peripheral).await? {
                return Ok(peripheral);
            }
        }
    }

    async fn matches(&self, peripheral: &Peripheral) -> Result<bool, BleError> {
        let Some(properties) = peripheral.properties().await? else {
            return Ok(false);
        };
        if !properties
            .address
            .to_string()
            .eq_ignore_ascii_case(&self.key_file.address)
        {
            return Ok(false);
        }
        let Some(service_data) = properties.service_data.get(&SERVICE) else {
            return Ok(false);
        };
        let manufacturer = properties
            .manufacturer_data
            .iter()
            .map(|(company, value)| (*company, value.as_slice()));
        Ok(
            advertisement::decode(manufacturer, service_data).is_some_and(|decoded| {
                decoded.serial == self.key_file.serial && decoded.model == self.key_file.model
            }),
        )
    }

    /// Connects, discovers services, and subscribes to notifications.
    async fn open(
        &self,
        peripheral: Peripheral,
        events: Events,
        limit: Duration,
    ) -> Result<Connection, BleError> {
        bounded(limit, "connecting", peripheral.connect()).await?;
        bounded(
            limit,
            "discovering services",
            peripheral.discover_services(),
        )
        .await?;
        let characteristics = peripheral.characteristics();
        let find = |uuid| {
            characteristics
                .iter()
                .find(|characteristic| characteristic.uuid == uuid)
                .cloned()
                .ok_or(BleError::MissingCharacteristic(uuid))
        };
        let command = find(COMMAND)?;
        let notify = find(NOTIFY)?;
        let notifications = peripheral.notifications().await?;
        bounded(limit, "subscribing", peripheral.subscribe(&notify)).await?;
        Ok(Connection {
            id: peripheral.id(),
            peripheral,
            command,
            notifications,
            events,
            key: self.key_file.key.clone(),
        })
    }
}

impl Connector for Unit {
    type Link = Connection;

    async fn connect(&mut self, limit: Duration) -> Result<Connection, BleError> {
        // Subscribe to events before scanning so no advertisement is missed.
        let mut events = self.adapter.events().await?;
        self.adapter.start_scan(ScanFilter::default()).await?;
        let found = timeout(limit, self.find(&mut events)).await;
        self.adapter.stop_scan().await?;
        let peripheral = found.map_err(|_| BleError::NotFound)??;
        match self.open(peripheral.clone(), events, limit).await {
            Ok(connection) => Ok(connection),
            Err(error) => {
                // The setup error matters more than a failed cleanup.
                peripheral.disconnect().await.ok();
                Err(error)
            }
        }
    }
}

/// One connection with its notification subscription.
struct Connection {
    id: PeripheralId,
    peripheral: Peripheral,
    command: Characteristic,
    notifications: Notifications,
    /// Adapter events, watched for this peripheral's disconnect.
    events: Events,
    key: BleKey,
}

impl Link for Connection {
    async fn exchange(
        &mut self,
        request: &Request,
        limit: Duration,
    ) -> Result<RawStatus, BleError> {
        let mut decoder = ResponseDecoder::new(self.key.clone(), request.action);
        let mask = NonZeroU8::new(rand::random_range(1..=u8::MAX)).expect("mask is nonzero");
        self.peripheral
            .write(
                &self.command,
                &request.encode(&self.key, mask),
                WriteType::WithoutResponse,
            )
            .await?;
        let outcome = timeout(limit, async {
            loop {
                tokio::select! {
                    notification = self.notifications.next() => {
                        let notification = notification.ok_or(BleError::Disconnected)?;
                        if notification.uuid == NOTIFY
                            && let Some(status) = decoder.feed(&notification.value)?
                        {
                            return Ok(status);
                        }
                    }
                    event = self.events.next() => {
                        if is_disconnect(event.as_ref(), &self.id) {
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
                        if is_disconnect(event.as_ref(), &self.id) {
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
        // Disconnect stops notifications, so there is no unsubscribe.  An
        // error here means the link is already gone.
        self.peripheral.disconnect().await.ok();
    }
}

/// A disconnect of `id`, or the end of the event stream.
fn is_disconnect(event: Option<&CentralEvent>, id: &PeripheralId) -> bool {
    match event {
        None => true,
        Some(CentralEvent::DeviceDisconnected(disconnected)) => disconnected == id,
        Some(_) => false,
    }
}

async fn bounded<T>(
    limit: Duration,
    action: &'static str,
    operation: impl Future<Output = Result<T, btleplug::Error>>,
) -> Result<T, BleError> {
    Ok(timeout(limit, operation)
        .await
        .map_err(|_| BleError::TimedOut(action))??)
}
