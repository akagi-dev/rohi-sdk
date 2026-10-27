///////////////////////////////////////////////////////////////////////////////
//
//  Copyright 2025-2026 Akagi Engineering <admin@akagi.dev>
//
//  Licensed under the Apache License, Version 2.0 (the "License");
//  you may not use this file except in compliance with the License.
//  You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.
//
///////////////////////////////////////////////////////////////////////////////

//! Persistent configuration service.
//!
//! The [`ConfigStore`] on the `rohi-cfg` flash partition has exactly one
//! owner: the configuration task started by [`start`]. Everything else (the
//! Wi-Fi boot logic today, the control layer later) talks to it through the
//! cloneable [`ConfigHandle`] and a bounded request channel, so nothing outside
//! this module touches the raw configuration flash and there is no global
//! mutable store.
//!
//! ```text
//! caller -> ConfigHandle -> requests (bounded) -> config task -> ConfigStore -> flash
//!        <-                 responses            <-
//! ```
//!
//! Feature code defines its own `ConfigSection` types and uses the typed
//! [`ConfigHandle::get`], [`ConfigHandle::set`] and [`ConfigHandle::remove`].
//! Committed-but-corrupted configuration never looks like an empty one: the
//! service then reports [`Unavailable`] for every request instead of pretending
//! nothing was provisioned. The only way out is the explicit, separate
//! [`StorageMaintenance::format_storage`]: it formats the region and the service
//! becomes ready with an empty store.
//!
//! There is deliberately no generic "erase every section" operation on
//! [`ConfigHandle`]: ordinary configuration maintenance (`remove` of one
//! section) must never touch unrelated or future protected sections such as an
//! identity. The full factory reset is an explicit, privileged operation of its
//! own: [`StorageMaintenance::factory_reset`] formats the whole region.
//!
//! Persistent Wi-Fi configuration is *state*: after the owner task committed a
//! `WifiConfig` or `NetworkConfig` section (and once at boot, from the stored
//! values) it publishes the value to the Wi-Fi manager as the desired
//! configuration, synchronously, in the same task step as the commit. The `Identity` section works the same
//! way: the owner updates the signer in the commit step. So does
//! `LocationConfig`: the shared location is updated for the telemetry frames and
//! the `location` service. So does
//! `ConnectivityConfig`: the telemetry task is signalled and restarts with the
//! new values. So does `NtpServersConfig`: the NTP task restarts with the new
//! servers. There is no `.await` between commit and
//! publication, so a caller that is cancelled while waiting for the response
//! cannot leave a stored configuration unknown to the runtime.
//!
//! Flash erase and programming are synchronous and `esp-storage` runs them in
//! a critical section with the flash cache disabled, so an update stalls the
//! whole CPU (each 4 KiB sector erase takes tens of milliseconds), not only
//! this task. Another executor or priority would not help; configuration
//! updates are rare and must not be issued from timing-critical paths.

use core::fmt;

use embassy_executor::Spawner;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use esp_hal::peripherals::FLASH;
use log::{error, info, warn};
use rohi_config::{
    ConfigError, ConfigOpenState, ConfigSection, ConfigStore, Corruption, EncodeError,
    MAX_SECTION_PAYLOAD, SectionError, SectionId,
};
use rohi_config_esp::esp::{PartitionError, config_storage, esp_storage::FlashStorage};
use rohi_config_esp::{EspConfigStorage, EspStorageError};

use rohi_connectivity::ConnectivityConfig;
use rohi_identity::Identity;
use rohi_location::LocationConfig;
use rohi_net::{NetworkConfig, WifiConfig, WifiHandle};
use rohi_time_ntp::NtpServersConfig;

use crate::tasks::RawMutex;
use crate::{connectivity, identity, location, time};

/// Configuration storage of the Altruist Urban.
pub type Storage = EspConfigStorage<FlashStorage<'static>>;

/// Why the configuration store is not usable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// The `rohi-cfg` partition is missing or unusable (wrong partition table?).
    Partition(PartitionError),
    /// The store could not be opened. [`ConfigError::CorruptStorage`] means
    /// committed configuration exists but no slot is valid.
    Open(ConfigError<EspStorageError>),
}

impl Unavailable {
    /// `true` if committed configuration exists but is unreadable.
    pub fn is_corrupt(&self) -> bool {
        matches!(self, Self::Open(ConfigError::CorruptStorage(_)))
    }

    /// The corruption reason, if the flash holds corrupted configuration.
    pub fn corruption(&self) -> Option<Corruption> {
        match self {
            Self::Open(ConfigError::CorruptStorage(reason)) => Some(*reason),
            _ => None,
        }
    }
}

impl fmt::Display for Unavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Partition(error) => write!(f, "configuration partition: {error:?}"),
            Self::Open(error) => write!(f, "{error}"),
        }
    }
}

/// State of the configuration store as seen by the runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigStatus {
    /// The store is serving requests; the current [`ConfigStore::state`].
    Ready(ConfigOpenState),
    /// Every request but [`StorageMaintenance::format_storage`] fails with
    /// [`ServiceError::Unavailable`].
    Unavailable(Unavailable),
}

/// Failure of a configuration request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceError {
    Unavailable(Unavailable),
    /// [`StorageMaintenance::format_storage`] refused: the store is healthy.
    StorageHealthy,
    Store(ConfigError<EspStorageError>),
    /// The stored section could not be decoded into the requested type.
    Section(SectionError),
    /// The value could not be encoded into a section payload.
    Encode(EncodeError),
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(reason) => write!(f, "configuration unavailable: {reason}"),
            Self::StorageHealthy => f.write_str("configuration storage is healthy, not formatting"),
            Self::Store(error) => write!(f, "{error}"),
            Self::Section(error) => write!(f, "{error}"),
            Self::Encode(error) => write!(f, "{error}"),
        }
    }
}

/// An encoded section payload crossing the request channel.
#[derive(Clone)]
struct Blob {
    version: u16,
    len: usize,
    bytes: [u8; MAX_SECTION_PAYLOAD],
}

impl Blob {
    const fn empty() -> Self {
        Self {
            version: 0,
            len: 0,
            bytes: [0; MAX_SECTION_PAYLOAD],
        }
    }

    fn payload(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

// Boxing is not an option without a heap; the channels hold one message each.
#[allow(clippy::large_enum_variant)]
enum Command {
    Status,
    Get(SectionId),
    Set { id: SectionId, blob: Blob },
    Remove(SectionId),
    FormatStorage,
    FactoryReset,
}

#[allow(clippy::large_enum_variant)]
enum Outcome {
    Status(ConfigStatus),
    Section(Blob),
    NotFound,
    Done,
    Failed(ServiceError),
}

struct Request {
    seq: u32,
    command: Command,
}

struct Response {
    seq: u32,
    outcome: Outcome,
}

/// The request channel pair and the lock serializing callers.
///
/// Statics only exist to give the owner task and the handles a common place;
/// the store itself lives in the task.
pub struct ConfigService {
    requests: Channel<RawMutex, Request, 1>,
    responses: Channel<RawMutex, Response, 1>,
    /// Held for the whole call; the value numbers the requests so that the
    /// response of an abandoned call is never mistaken for a later one.
    call: Mutex<RawMutex, u32>,
}

impl ConfigService {
    const fn new() -> Self {
        Self {
            requests: Channel::new(),
            responses: Channel::new(),
            call: Mutex::new(0),
        }
    }
}

static SERVICE: ConfigService = ConfigService::new();

/// Cloneable handle to the configuration owner task.
#[derive(Clone, Copy)]
pub struct ConfigHandle {
    service: &'static ConfigService,
}

impl ConfigHandle {
    async fn call(&self, command: Command) -> Outcome {
        let mut seq = self.service.call.lock().await;
        *seq = seq.wrapping_add(1);
        let seq = *seq;
        self.service.requests.send(Request { seq, command }).await;
        loop {
            let response = self.service.responses.receive().await;
            if response.seq == seq {
                return response.outcome;
            }
        }
    }

    /// State of the store, for diagnostics and policy decisions.
    pub async fn status(&self) -> ConfigStatus {
        match self.call(Command::Status).await {
            Outcome::Status(status) => status,
            _ => unreachable!("status request answered with another outcome"),
        }
    }

    /// Loads section `T`, `None` if it was never stored.
    pub async fn get<T: ConfigSection>(&self) -> Result<Option<T>, ServiceError> {
        match self.call(Command::Get(T::ID)).await {
            Outcome::Section(blob) => T::decode_versioned(blob.version, blob.payload())
                .map(Some)
                .map_err(ServiceError::Section),
            Outcome::NotFound => Ok(None),
            Outcome::Failed(error) => Err(error),
            _ => unreachable!("get request answered with another outcome"),
        }
    }

    /// Stores `value`, replacing the previous section `T`.
    ///
    /// Cancellation: dropping the future before the request is queued leaves
    /// the store unchanged; afterwards the owner task still commits it (and
    /// publishes a `NetworkConfig` to the Wi-Fi runtime), so the operation is
    /// logically completed and the caller only misses the result.
    pub async fn set<T: ConfigSection>(&self, value: &T) -> Result<(), ServiceError> {
        let mut blob = Blob::empty();
        blob.version = T::VERSION;
        blob.len =
            rohi_config::encode_to_slice(value, &mut blob.bytes).map_err(ServiceError::Encode)?;
        let command = Command::Set { id: T::ID, blob };
        match self.call(command).await {
            Outcome::Done => Ok(()),
            Outcome::Failed(error) => Err(error),
            _ => unreachable!("set request answered with another outcome"),
        }
    }

    /// Removes section `T`; a no-op if it is absent.
    ///
    /// Cancellation: same as [`ConfigHandle::set`].
    pub async fn remove<T: ConfigSection>(&self) -> Result<(), ServiceError> {
        match self.call(Command::Remove(T::ID)).await {
            Outcome::Done => Ok(()),
            Outcome::Failed(error) => Err(error),
            _ => unreachable!("remove request answered with another outcome"),
        }
    }
}

/// Low-level operations on the whole configuration storage: recovery of
/// unusable storage and factory reset.
///
/// Kept apart from [`ConfigHandle`] on purpose: they are not configuration
/// mutations of one section and must not be reachable from ordinary section
/// maintenance.
#[derive(Clone, Copy)]
pub struct StorageMaintenance {
    service: &'static ConfigService,
}

impl StorageMaintenance {
    /// Formats the configuration region of an unavailable (corrupt or
    /// unreadable) store, discarding everything in it; the service then becomes
    /// ready with an empty store.
    ///
    /// A ready store is never touched: this fails with
    /// [`ServiceError::StorageHealthy`]. A missing partition cannot be
    /// formatted and fails with [`ServiceError::Unavailable`].
    pub async fn format_storage(&self) -> Result<(), ServiceError> {
        let handle = ConfigHandle {
            service: self.service,
        };
        match handle.call(Command::FormatStorage).await {
            Outcome::Done => Ok(()),
            Outcome::Failed(error) => Err(error),
            _ => unreachable!("format request answered with another outcome"),
        }
    }

    /// Factory reset: formats the configuration region whatever its state,
    /// healthy or not, and returns every runtime consumer (Wi-Fi, identity,
    /// connectivity, NTP, location) to its defaults in the same step.
    ///
    /// Everything is discarded: the sections of this firmware, the identity,
    /// sections it does not know and the generation counter. A missing
    /// partition cannot be formatted and fails with [`ServiceError::Unavailable`].
    pub async fn factory_reset(&self) -> Result<(), ServiceError> {
        let handle = ConfigHandle {
            service: self.service,
        };
        match handle.call(Command::FactoryReset).await {
            Outcome::Done => Ok(()),
            Outcome::Failed(error) => Err(error),
            _ => unreachable!("factory reset request answered with another outcome"),
        }
    }
}

/// What the owner task holds.
#[allow(clippy::large_enum_variant)]
enum Owner {
    Ready(ConfigStore<Storage>),
    /// The storage is kept whenever the partition exists, so that
    /// [`Command::FormatStorage`] can format it.
    Unavailable {
        reason: Unavailable,
        storage: Option<Storage>,
    },
}

impl Owner {
    fn handle(&mut self, command: Command, wifi: WifiHandle<'static, RawMutex>) -> Outcome {
        if let Command::FormatStorage = command {
            return self.format_storage();
        }
        if let Command::FactoryReset = command {
            return self.factory_reset(wifi);
        }
        match self {
            Self::Unavailable { reason, .. } => match command {
                Command::Status => Outcome::Status(ConfigStatus::Unavailable(reason.clone())),
                _ => Outcome::Failed(ServiceError::Unavailable(reason.clone())),
            },
            Self::Ready(store) => match command {
                Command::Status => Outcome::Status(ConfigStatus::Ready(store.state())),
                Command::Get(id) => {
                    let mut blob = Blob::empty();
                    match store.read_raw(id, &mut blob.bytes) {
                        Ok(Some(info)) => {
                            blob.version = info.version;
                            blob.len = info.length;
                            Outcome::Section(blob)
                        }
                        Ok(None) => Outcome::NotFound,
                        Err(error) => Outcome::Failed(ServiceError::Store(error)),
                    }
                }
                Command::Set { id, blob } => {
                    match store.set_raw(id, blob.version, blob.payload()) {
                        Ok(()) => {
                            publish_committed(wifi, id, Some(&blob));
                            Outcome::Done
                        }
                        Err(error) => Outcome::Failed(ServiceError::Store(error)),
                    }
                }
                Command::Remove(id) => match store.remove_raw(id) {
                    Ok(()) => {
                        publish_committed(wifi, id, None);
                        Outcome::Done
                    }
                    Err(error) => Outcome::Failed(ServiceError::Store(error)),
                },
                Command::FormatStorage | Command::FactoryReset => unreachable!("handled above"),
            },
        }
    }

    /// Formats the region whatever its state, then returns the runtime to the
    /// defaults. Unlike [`Self::format_storage`] it also acts on a healthy
    /// store, so it erases the identity and sections unknown to this firmware.
    fn factory_reset(&mut self, wifi: WifiHandle<'static, RawMutex>) -> Outcome {
        if let Self::Unavailable {
            reason,
            storage: None,
        } = self
        {
            return Outcome::Failed(ServiceError::Unavailable(reason.clone()));
        }
        // Every path below puts a real state back.
        let taken = core::mem::replace(
            self,
            Self::Unavailable {
                reason: Unavailable::Partition(PartitionError::NotFound),
                storage: None,
            },
        );
        let raw = match taken {
            Self::Ready(store) => store.into_storage(),
            Self::Unavailable {
                storage: Some(raw), ..
            } => raw,
            Self::Unavailable { .. } => unreachable!("checked above"),
        };
        match ConfigStore::format(raw) {
            Ok(store) => {
                warn!("Configuration: factory reset, region formatted");
                *self = Self::Ready(store);
                for id in [
                    WifiConfig::ID,
                    NetworkConfig::ID,
                    Identity::ID,
                    ConnectivityConfig::ID,
                    NtpServersConfig::ID,
                    LocationConfig::ID,
                ] {
                    publish_committed(wifi, id, None);
                }
                Outcome::Done
            }
            Err(failure) => {
                error!("Configuration: factory reset failed: {}", failure.error);
                let error = failure.error.clone();
                *self = Self::Unavailable {
                    reason: Unavailable::Open(failure.error),
                    storage: Some(failure.storage),
                };
                Outcome::Failed(ServiceError::Store(error))
            }
        }
    }

    fn format_storage(&mut self) -> Outcome {
        match self {
            Self::Ready(_) => Outcome::Failed(ServiceError::StorageHealthy),
            Self::Unavailable { reason, storage } => {
                let Some(raw) = storage.take() else {
                    return Outcome::Failed(ServiceError::Unavailable(reason.clone()));
                };
                match ConfigStore::format(raw) {
                    Ok(store) => {
                        warn!("Configuration: region formatted, previous configuration discarded");
                        *self = Self::Ready(store);
                        Outcome::Done
                    }
                    Err(failure) => {
                        error!("Configuration: format failed: {}", failure.error);
                        *storage = Some(failure.storage);
                        Outcome::Failed(ServiceError::Store(failure.error))
                    }
                }
            }
        }
    }
}

fn open(flash: FLASH<'static>) -> Owner {
    let storage = match config_storage(FlashStorage::new(flash)) {
        Ok(storage) => storage,
        Err(error) => {
            error!("Configuration partition unavailable: {:?}", error);
            return Owner::Unavailable {
                reason: Unavailable::Partition(error),
                storage: None,
            };
        }
    };
    let region = storage.region();
    let location = (region.offset, region.length);
    let store = match ConfigStore::try_open(storage) {
        Ok(store) => store,
        Err(failure) => {
            match failure.error {
                ConfigError::CorruptStorage(reason) => error!(
                    "Configuration is CORRUPT ({}): committed data exists but is unreadable, \
                     not treating it as empty (format_storage recovers it)",
                    reason
                ),
                ref error => error!("Configuration store unavailable: {}", error),
            }
            return Owner::Unavailable {
                reason: Unavailable::Open(failure.error),
                storage: Some(failure.storage),
            };
        }
    };
    match store.open_state() {
        ConfigOpenState::Empty => {
            warn!("Configuration: empty @ {:#x}+{:#x}", location.0, location.1)
        }
        ConfigOpenState::Valid => info!(
            "Configuration: {} section(s), generation {} @ {:#x}+{:#x}",
            store.section_count(),
            store.generation().unwrap_or(0),
            location.0,
            location.1
        ),
        ConfigOpenState::Recovered => warn!(
            "Configuration: ignored a damaged slot, using generation {} (slot {}) @ {:#x}+{:#x}, \
             {} section(s); a lost newer update cannot be ruled out",
            store.generation().unwrap_or(0),
            store.active_slot().unwrap_or(0),
            location.0,
            location.1,
            store.section_count()
        ),
    }
    Owner::Ready(store)
}

/// Owns the [`ConfigStore`] and answers requests, one at a time.
#[embassy_executor::task]
async fn config_task(
    service: &'static ConfigService,
    mut owner: Owner,
    wifi: WifiHandle<'static, RawMutex>,
) {
    loop {
        let Request { seq, command } = service.requests.receive().await;
        let outcome = owner.handle(command, wifi);
        service.responses.send(Response { seq, outcome }).await;
    }
}

/// Opens the configuration store on the `rohi-cfg` partition and starts its
/// owner task. The returned handle is the only way to reach the configuration.
///
/// A missing partition or corrupted committed configuration does not stop the
/// firmware: the service keeps running and answers every request with
/// [`ServiceError::Unavailable`], so corruption is never mistaken for an empty
/// (unprovisioned) configuration. [`maintenance`] can format it.
///
/// `wifi` receives the desired Wi-Fi configuration: the stored one now, and
/// every later commit of the `WifiConfig` and `NetworkConfig` sections, see the
/// module documentation.
pub fn start(
    spawner: Spawner,
    flash: FLASH<'static>,
    wifi: WifiHandle<'static, RawMutex>,
) -> ConfigHandle {
    let mut owner = open(flash);
    publish_stored_wifi(&mut owner, wifi);
    publish_stored_location(&mut owner, wifi);
    spawner.spawn(config_task(&SERVICE, owner, wifi).unwrap());
    ConfigHandle { service: &SERVICE }
}

/// Handle for recovering unusable configuration storage, see
/// [`StorageMaintenance`]. Not part of the normal runtime API.
pub fn maintenance() -> StorageMaintenance {
    StorageMaintenance { service: &SERVICE }
}

/// Publishes a just committed (or removed) section to the runtime that
/// depends on it: `WifiConfig` and `NetworkConfig` become the desired Wi-Fi
/// and IPv4 configuration,
/// `Identity` becomes the signing identity, `ConnectivityConfig` is handed to
/// the telemetry task, `NtpServersConfig` to the NTP task. Synchronous on
/// purpose, see the module documentation. Other sections have no runtime consumer.
fn publish_committed(wifi: WifiHandle<'static, RawMutex>, id: SectionId, blob: Option<&Blob>) {
    if id == Identity::ID {
        match blob {
            None => identity::set_signer(None),
            Some(blob) => match Identity::decode_versioned(blob.version, blob.payload()) {
                Ok(identity) => identity::set_signer(Some(identity.signer())),
                Err(error) => warn!("Identity: committed section cannot be decoded: {}", error),
            },
        }
        return;
    }
    if id == ConnectivityConfig::ID {
        match blob {
            None => connectivity::publish(None),
            Some(blob) => {
                match ConnectivityConfig::decode_versioned(blob.version, blob.payload()) {
                    Ok(config) => connectivity::publish(Some(config)),
                    Err(error) => warn!(
                        "Connectivity: committed configuration cannot be decoded: {}",
                        error
                    ),
                }
            }
        }
        return;
    }
    if id == NtpServersConfig::ID {
        match blob {
            None => time::publish(None),
            Some(blob) => match NtpServersConfig::decode_versioned(blob.version, blob.payload()) {
                Ok(config) => time::publish(Some(config)),
                Err(error) => warn!("Time: committed NTP servers cannot be decoded: {}", error),
            },
        }
        return;
    }
    if id == LocationConfig::ID {
        match blob {
            None => location::shared().publish_configured(None),
            Some(blob) => match LocationConfig::decode_versioned(blob.version, blob.payload()) {
                Ok(config) => location::shared().publish_configured(Some(&config)),
                Err(error) => warn!(
                    "Location: committed configuration cannot be decoded: {}",
                    error
                ),
            },
        }
        return;
    }
    if id == WifiConfig::ID {
        match blob {
            None => wifi.set_wifi_config(None),
            Some(blob) => match WifiConfig::decode_versioned(blob.version, blob.payload()) {
                Ok(config) => wifi.set_wifi_config(Some(config)),
                Err(error) => warn!(
                    "Wi-Fi: committed configuration cannot be decoded: {}",
                    error
                ),
            },
        }
        return;
    }
    if id != NetworkConfig::ID {
        return;
    }
    match blob {
        None => wifi.set_network_config(NetworkConfig::new()),
        Some(blob) => match NetworkConfig::decode_versioned(blob.version, blob.payload()) {
            Ok(config) => wifi.set_network_config(config),
            Err(error) => warn!(
                "Network: committed configuration cannot be decoded: {}",
                error
            ),
        },
    }
}

/// Publishes the stored location at boot; without one the device does not
/// know where it is and telemetry reports that.
fn publish_stored_location(owner: &mut Owner, wifi: WifiHandle<'static, RawMutex>) {
    let Owner::Ready(store) = owner else {
        return;
    };
    let mut blob = Blob::empty();
    match store.read_raw(LocationConfig::ID, &mut blob.bytes) {
        Ok(Some(info)) => {
            blob.version = info.version;
            blob.len = info.length;
            publish_committed(wifi, LocationConfig::ID, Some(&blob));
        }
        Ok(None) => info!("Location: not set"),
        Err(error) => warn!("Location configuration unreadable: {}", error),
    }
}

/// Publishes the stored Wi-Fi and network configuration at boot. Without a
/// Wi-Fi one the manager stays `NotConfigured` and attempts nothing; without a
/// network one the address comes from DHCP.
fn publish_stored_wifi(owner: &mut Owner, wifi: WifiHandle<'static, RawMutex>) {
    let Owner::Ready(store) = owner else {
        warn!("Wi-Fi: configuration unavailable, not configured");
        return;
    };
    let mut blob = Blob::empty();
    match store.read_raw(NetworkConfig::ID, &mut blob.bytes) {
        Ok(Some(info)) => {
            blob.version = info.version;
            blob.len = info.length;
            publish_committed(wifi, NetworkConfig::ID, Some(&blob));
        }
        Ok(None) => info!("Network: default address settings"),
        Err(error) => warn!("Network configuration unreadable: {}", error),
    }
    match store.read_raw(WifiConfig::ID, &mut blob.bytes) {
        Ok(Some(info)) => {
            blob.version = info.version;
            blob.len = info.length;
            publish_committed(wifi, WifiConfig::ID, Some(&blob));
        }
        Ok(None) => info!("Wi-Fi: not configured"),
        Err(error) => warn!("Wi-Fi configuration unreadable: {}", error),
    }
}
