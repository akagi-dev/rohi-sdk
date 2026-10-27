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
//! ESP32 BLE peripheral backend.
//!
//! `esp-radio` provides the HCI controller, TrouBLE the host stack and GATT
//! server. This module turns them into the portable [`BleTransport`]: a single
//! connected central seen as a bounded byte pipe.
//!
//! ```text
//! central --write--> RX characteristic --> [RX queue] --> EspBle::recv
//! central <-notify-- TX characteristic <-- [TX queue] <-- EspBle::send
//! ```
//!
//! Stack callbacks never run application logic: GATT writes are copied into
//! the bounded RX queue (the ATT response is delayed until there is room, which
//! is the backpressure) and responses leave through the bounded TX queue.
//! Every connection gets a new epoch; chunks of an earlier connection are
//! never delivered to or sent for a later one.

use core::cell::{Cell, RefCell};

use embassy_futures::join::join;
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use esp_hal::efuse::base_mac_address;
use esp_radio::ble::controller::BleConnector;
use heapless::Vec;
use log::{debug, info, warn};
use rohi_net::{
    BleConfig, BleConnectionInfo, BleError, BleName, BleProbe, BleState, BleTransport,
    DEFAULT_BLE_PAYLOAD, MAX_BLE_PAYLOAD,
};
use trouble_host::prelude::*;

use crate::wireless::WirelessError;

/// ATT header of a write or notification.
const ATT_OVERHEAD: u16 = 3;

#[derive(Clone, Copy)]
struct Chunk {
    epoch: u32,
    len: u8,
    data: [u8; MAX_BLE_PAYLOAD],
}

impl Chunk {
    fn new(epoch: u32, data: &[u8]) -> Self {
        let mut chunk = Self {
            epoch,
            len: data.len() as u8,
            data: [0; MAX_BLE_PAYLOAD],
        };
        chunk.data[..data.len()].copy_from_slice(data);
        chunk
    }

    fn bytes(&self) -> &[u8] {
        &self.data[..usize::from(self.len)]
    }
}

#[allow(clippy::large_enum_variant)]
enum RxEvent {
    Data(Chunk),
    Disconnected,
}

#[derive(Clone, Copy)]
struct Link {
    state: BleState,
    epoch: u32,
    payload: usize,
    info: Option<BleConnectionInfo>,
}

/// State shared by [`EspBle`] and [`BleRunner`]: the bounded RX and TX queues
/// and the published link properties.
struct Shared<const RX_DEPTH: usize, const TX_DEPTH: usize> {
    rx: Channel<CriticalSectionRawMutex, RxEvent, RX_DEPTH>,
    tx: Channel<CriticalSectionRawMutex, Chunk, TX_DEPTH>,
    link: Mutex<CriticalSectionRawMutex, Cell<Link>>,
    /// The advertised name, empty until the runner started.
    name: Mutex<CriticalSectionRawMutex, RefCell<heapless::String<NAME_MAX>>>,
}

impl<const RX_DEPTH: usize, const TX_DEPTH: usize> Shared<RX_DEPTH, TX_DEPTH> {
    const fn new() -> Self {
        Self {
            rx: Channel::new(),
            tx: Channel::new(),
            link: Mutex::new(Cell::new(Link {
                state: BleState::Disabled,
                epoch: 0,
                payload: DEFAULT_BLE_PAYLOAD,
                info: None,
            })),
            name: Mutex::new(RefCell::new(heapless::String::new())),
        }
    }

    fn link(&self) -> Link {
        self.link.lock(Cell::get)
    }

    fn update_link(&self, f: impl FnOnce(&mut Link)) {
        self.link.lock(|cell| {
            let mut value = cell.get();
            f(&mut value);
            cell.set(value);
        });
    }

    fn set_name(&self, name: &str) {
        self.name.lock(|cell| {
            let mut stored = cell.borrow_mut();
            stored.clear();
            let _ = stored.push_str(name);
        });
    }
}

/// Statically allocated storage and bounds of the BLE peripheral. Place it in
/// a `static` (for example with `static_cell::StaticCell`) and pass it to
/// [`EspWireless::ble`](crate::EspWireless::ble). Every bound is a const
/// generic so the firmware states its own RAM budget:
///
/// - `RX_DEPTH`: chunks buffered from the central before its write is held
///   back (the ATT response is delayed, which is the backpressure).
/// - `TX_DEPTH`: chunks buffered for notification; `send` waits when full.
/// - `CONNECTIONS`: simultaneous connections of the host stack.
/// - `L2CAP_CHANNELS`: L2CAP channels of the host stack (signalling and ATT
///   per connection).
/// - `ATTRIBUTES`: entries of the GATT attribute table: GAP, GATT and the
///   ROHI service with its characteristics and descriptors. TrouBLE panics
///   when the BLE task starts if the table is too small.
/// - `CONTROLLER_SLOTS`: HCI command slots of the controller.
pub struct BleResources<
    const RX_DEPTH: usize,
    const TX_DEPTH: usize,
    const CONNECTIONS: usize,
    const L2CAP_CHANNELS: usize,
    const ATTRIBUTES: usize,
    const CONTROLLER_SLOTS: usize,
> {
    shared: Shared<RX_DEPTH, TX_DEPTH>,
}

impl<
    const RX_DEPTH: usize,
    const TX_DEPTH: usize,
    const CONNECTIONS: usize,
    const L2CAP_CHANNELS: usize,
    const ATTRIBUTES: usize,
    const CONTROLLER_SLOTS: usize,
> BleResources<RX_DEPTH, TX_DEPTH, CONNECTIONS, L2CAP_CHANNELS, ATTRIBUTES, CONTROLLER_SLOTS>
{
    const BUDGET_OK: () = assert!(
        RX_DEPTH > 0
            && TX_DEPTH > 0
            && CONNECTIONS > 0
            && L2CAP_CHANNELS >= 2
            && CONTROLLER_SLOTS > 0,
        "BLE queues need room, at least one connection, two L2CAP channels and one controller slot"
    );

    pub const fn new() -> Self {
        let () = Self::BUDGET_OK;
        Self {
            shared: Shared::new(),
        }
    }
}

impl<
    const RX_DEPTH: usize,
    const TX_DEPTH: usize,
    const CONNECTIONS: usize,
    const L2CAP_CHANNELS: usize,
    const ATTRIBUTES: usize,
    const CONTROLLER_SLOTS: usize,
> Default
    for BleResources<RX_DEPTH, TX_DEPTH, CONNECTIONS, L2CAP_CHANNELS, ATTRIBUTES, CONTROLLER_SLOTS>
{
    fn default() -> Self {
        Self::new()
    }
}

/// The connected central as a [`BleTransport`].
pub struct EspBle<const RX_DEPTH: usize, const TX_DEPTH: usize> {
    shared: &'static Shared<RX_DEPTH, TX_DEPTH>,
}

impl<const RX_DEPTH: usize, const TX_DEPTH: usize> EspBle<RX_DEPTH, TX_DEPTH> {
    /// A copyable view of the peripheral for the `ble` service. The transport
    /// itself belongs to the control link, so take the view before handing it
    /// over.
    pub fn status(&self) -> EspBleStatus<RX_DEPTH, TX_DEPTH> {
        EspBleStatus {
            shared: self.shared,
        }
    }
}

/// Read-only view of the BLE peripheral, see [`EspBle::status`].
pub struct EspBleStatus<const RX_DEPTH: usize, const TX_DEPTH: usize> {
    shared: &'static Shared<RX_DEPTH, TX_DEPTH>,
}

impl<const RX_DEPTH: usize, const TX_DEPTH: usize> Clone for EspBleStatus<RX_DEPTH, TX_DEPTH> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<const RX_DEPTH: usize, const TX_DEPTH: usize> Copy for EspBleStatus<RX_DEPTH, TX_DEPTH> {}

impl<const RX_DEPTH: usize, const TX_DEPTH: usize> BleProbe for EspBleStatus<RX_DEPTH, TX_DEPTH> {
    fn state(&self) -> BleState {
        self.shared.link().state
    }

    fn max_payload(&self) -> usize {
        self.shared.link().payload
    }

    fn name(&self) -> Option<BleName> {
        self.shared
            .name
            .lock(|cell| BleName::new(cell.borrow().as_str()))
    }
}

impl<const RX_DEPTH: usize, const TX_DEPTH: usize> BleTransport for EspBle<RX_DEPTH, TX_DEPTH> {
    /// Cancel-safe: a chunk is taken from the queue only when it is returned.
    async fn recv(&mut self, out: &mut [u8]) -> Result<usize, BleError> {
        match self.shared.rx.receive().await {
            RxEvent::Disconnected => Err(BleError::Disconnected),
            RxEvent::Data(chunk) => {
                let data = chunk.bytes();
                if data.len() > out.len() {
                    return Err(BleError::BufferTooSmall);
                }
                out[..data.len()].copy_from_slice(data);
                Ok(data.len())
            }
        }
    }

    /// Cancel-safe: the chunk is queued entirely or not at all.
    async fn send(&mut self, data: &[u8]) -> Result<(), BleError> {
        let current = self.shared.link();
        if current.info.is_none() {
            return Err(BleError::Disconnected);
        }
        if data.len() > current.payload {
            return Err(BleError::BufferTooSmall);
        }
        self.shared.tx.send(Chunk::new(current.epoch, data)).await;
        Ok(())
    }

    fn connection_info(&self) -> Option<BleConnectionInfo> {
        self.shared.link().info
    }

    fn max_payload(&self) -> usize {
        self.shared.link().payload
    }

    fn state(&self) -> BleState {
        self.shared.link().state
    }

    fn connection_epoch(&self) -> u32 {
        self.shared.link().epoch
    }
}

/// Drives the BLE controller, host stack, advertising and GATT server.
/// Run it on its own task: `runner.run().await`.
pub struct BleRunner<
    const RX_DEPTH: usize,
    const TX_DEPTH: usize,
    const CONNECTIONS: usize,
    const L2CAP_CHANNELS: usize,
    const ATTRIBUTES: usize,
    const CONTROLLER_SLOTS: usize,
> {
    connector: BleConnector<'static>,
    config: BleConfig,
    shared: &'static Shared<RX_DEPTH, TX_DEPTH>,
}

pub(crate) fn new<
    const RX_DEPTH: usize,
    const TX_DEPTH: usize,
    const CONNECTIONS: usize,
    const L2CAP_CHANNELS: usize,
    const ATTRIBUTES: usize,
    const CONTROLLER_SLOTS: usize,
>(
    bt: esp_hal::peripherals::BT<'static>,
    config: BleConfig,
    resources: &'static mut BleResources<
        RX_DEPTH,
        TX_DEPTH,
        CONNECTIONS,
        L2CAP_CHANNELS,
        ATTRIBUTES,
        CONTROLLER_SLOTS,
    >,
) -> Result<
    (
        EspBle<RX_DEPTH, TX_DEPTH>,
        BleRunner<RX_DEPTH, TX_DEPTH, CONNECTIONS, L2CAP_CHANNELS, ATTRIBUTES, CONTROLLER_SLOTS>,
    ),
    WirelessError,
> {
    let connector = BleConnector::new(bt, Default::default()).map_err(|error| {
        warn!("BLE controller init failed: {:?}", error);
        WirelessError::BleInit
    })?;
    let shared: &'static Shared<RX_DEPTH, TX_DEPTH> = &resources.shared;
    shared.update_link(|link| link.state = BleState::Advertising);
    Ok((
        EspBle { shared },
        BleRunner {
            connector,
            config,
            shared,
        },
    ))
}

/// Longest device name: the whole scan response minus its 2-byte header.
const NAME_MAX: usize = 29;

/// The configured name, or `<prefix>-<last two MAC bytes>` by default, for
/// example `ALTRUIST-URBAN-A1B2`.
fn device_name(config: &BleConfig) -> heapless::String<NAME_MAX> {
    use core::fmt::Write;

    if let Some(name) = &config.name {
        let mut configured = heapless::String::new();
        let _ = configured.push_str(name.as_str());
        return configured;
    }
    let prefix = config.name_prefix;

    let mac = base_mac_address();
    let mac = mac.as_bytes();
    let suffix = if mac.len() >= 2 {
        [mac[mac.len() - 2], mac[mac.len() - 1]]
    } else {
        [0; 2]
    };
    let mut name = heapless::String::new();
    // Room for "-XXXX"; a longer prefix is cut on a character boundary.
    for c in prefix.chars() {
        if name.len() + c.len_utf8() > NAME_MAX - 5 {
            break;
        }
        let _ = name.push(c);
    }
    let _ = write!(name, "-{:02X}{:02X}", suffix[0], suffix[1]);
    name
}

/// Static random address derived from the factory MAC.
fn address() -> Address {
    let mut bytes: [u8; 6] = base_mac_address().as_bytes().try_into().unwrap_or([0; 6]);
    // The two most significant bits of a static random address are 1.
    bytes[5] |= 0xC0;
    Address::random(bytes)
}

impl<
    const RX_DEPTH: usize,
    const TX_DEPTH: usize,
    const CONNECTIONS: usize,
    const L2CAP_CHANNELS: usize,
    const ATTRIBUTES: usize,
    const CONTROLLER_SLOTS: usize,
> BleRunner<RX_DEPTH, TX_DEPTH, CONNECTIONS, L2CAP_CHANNELS, ATTRIBUTES, CONTROLLER_SLOTS>
{
    /// Runs the BLE stack forever. Errors restart advertising; they never
    /// stop the rest of the firmware.
    pub async fn run(self) -> ! {
        let controller: ExternalController<_, CONTROLLER_SLOTS> =
            ExternalController::new(self.connector);
        let shared = self.shared;
        let mut resources: HostResources<_, DefaultPacketPool, CONNECTIONS, L2CAP_CHANNELS> =
            HostResources::new();
        let stack = trouble_host::new(controller, &mut resources)
            .set_random_address(address())
            .build();
        let mut peripheral = stack.peripheral();
        let mut runner = stack.runner();

        let name = device_name(&self.config);
        shared.set_name(name.as_str());
        info!("BLE: device name {}", name.as_str());
        let mut rx_store = [0u8; MAX_BLE_PAYLOAD];
        let mut tx_store = [0u8; MAX_BLE_PAYLOAD];
        let mut table: AttributeTable<'_, CriticalSectionRawMutex, ATTRIBUTES> =
            AttributeTable::new();
        if GapConfig::default(name.as_str()).build(&mut table).is_err() {
            warn!("BLE: GAP service not created");
        }
        let (rx_handle, tx_handle) = {
            let mut service = table.add_service(Service::new(self.config.gatt.service));
            let rx = service
                .add_characteristic(
                    self.config.gatt.rx,
                    [
                        CharacteristicProp::Write,
                        CharacteristicProp::WriteWithoutResponse,
                    ],
                    Vec::<u8, MAX_BLE_PAYLOAD>::new(),
                    &mut rx_store,
                )
                .build();
            let tx = service
                .add_characteristic(
                    self.config.gatt.tx,
                    [CharacteristicProp::Notify],
                    Vec::<u8, MAX_BLE_PAYLOAD>::new(),
                    &mut tx_store,
                )
                .build();
            service.build();
            (rx, tx)
        };
        let server: AttributeServer<
            '_,
            CriticalSectionRawMutex,
            DefaultPacketPool,
            ATTRIBUTES,
            CONNECTIONS,
        > = AttributeServer::new(table);

        let host = async {
            loop {
                if let Err(error) = runner.run().await {
                    warn!("BLE host stopped: {:?}", error);
                    shared.update_link(|link| link.state = BleState::Error(BleError::DriverError));
                    embassy_time::Timer::after_secs(1).await;
                }
            }
        };

        let serve = async {
            loop {
                match advertise(
                    shared,
                    &self.config,
                    name.as_str(),
                    &mut peripheral,
                    &server,
                )
                .await
                {
                    Ok(connection) => {
                        serve_connection(shared, &connection, rx_handle.handle, &tx_handle).await;
                        end_connection(shared).await;
                    }
                    Err(error) => {
                        warn!("BLE advertising failed: {:?}", error);
                        shared.update_link(|link| {
                            link.state = BleState::Error(BleError::DriverError)
                        });
                        embassy_time::Timer::after_secs(1).await;
                    }
                }
            }
        };

        join(host, serve).await;
        unreachable!("BLE tasks never end")
    }
}

async fn advertise<
    'values,
    'server,
    C: Controller,
    const RX_DEPTH: usize,
    const TX_DEPTH: usize,
    const ATTRIBUTES: usize,
    const CONNECTIONS: usize,
>(
    shared: &Shared<RX_DEPTH, TX_DEPTH>,
    config: &BleConfig,
    name: &str,
    peripheral: &mut Peripheral<'values, C, DefaultPacketPool>,
    server: &'server AttributeServer<
        'values,
        CriticalSectionRawMutex,
        DefaultPacketPool,
        ATTRIBUTES,
        CONNECTIONS,
    >,
) -> Result<GattConnection<'values, 'server, DefaultPacketPool>, BleHostError<C::Error>> {
    let service = config.gatt.service.to_le_bytes();
    let mut advertising = [0u8; 31];
    let advertising_len = AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::IncompleteServiceUuids128(&[service]),
        ],
        &mut advertising[..],
    )?;
    // The name goes into the scan response, which has the whole 31 bytes.
    let mut scan_response = [0u8; 31];
    let name = name.as_bytes();
    let scan_len = AdStructure::encode_slice(
        &[AdStructure::CompleteLocalName(name)],
        &mut scan_response[..],
    )?;

    shared.update_link(|link| {
        link.state = BleState::Advertising;
        link.info = None;
    });
    let advertiser = peripheral
        .advertise(
            &Default::default(),
            Advertisement::ConnectableScannableUndirected {
                adv_data: &advertising[..advertising_len],
                scan_data: &scan_response[..scan_len],
            },
        )
        .await?;
    debug!("BLE: advertising");
    let connection = advertiser.accept().await?.with_attribute_server(server)?;
    Ok(connection)
}

fn connection_info(connection: &GattConnection<'_, '_, DefaultPacketPool>) -> BleConnectionInfo {
    match connection.raw().security_level() {
        Ok(SecurityLevel::Encrypted) => BleConnectionInfo {
            encrypted: true,
            authenticated: false,
        },
        Ok(SecurityLevel::EncryptedAuthenticated) => BleConnectionInfo {
            encrypted: true,
            authenticated: true,
        },
        _ => BleConnectionInfo::default(),
    }
}

fn refresh_link<const RX_DEPTH: usize, const TX_DEPTH: usize>(
    shared: &Shared<RX_DEPTH, TX_DEPTH>,
    connection: &GattConnection<'_, '_, DefaultPacketPool>,
) {
    let info = connection_info(connection);
    let mtu = connection.raw().att_mtu().saturating_sub(ATT_OVERHEAD);
    let payload = usize::from(mtu).clamp(DEFAULT_BLE_PAYLOAD, MAX_BLE_PAYLOAD);
    shared.update_link(|link| {
        link.info = Some(info);
        link.payload = payload;
        link.state = BleState::Connected(info);
    });
}

/// Serves one connection until it ends: receives writes and notifies
/// responses.
async fn serve_connection<const RX_DEPTH: usize, const TX_DEPTH: usize>(
    shared: &Shared<RX_DEPTH, TX_DEPTH>,
    connection: &GattConnection<'_, '_, DefaultPacketPool>,
    rx_handle: u16,
    tx: &Characteristic<Vec<u8, MAX_BLE_PAYLOAD>>,
) {
    // A new connection starts clean: nothing of the previous one survives.
    while shared.rx.try_receive().is_ok() {}
    while shared.tx.try_receive().is_ok() {}
    shared.update_link(|link| {
        link.epoch = link.epoch.wrapping_add(1);
        link.payload = DEFAULT_BLE_PAYLOAD;
    });
    refresh_link(shared, connection);
    info!("BLE: central connected");
    let epoch = shared.link().epoch;

    let events = async {
        loop {
            match connection.next().await {
                GattConnectionEvent::Disconnected { reason } => {
                    debug!("BLE: disconnected: {:?}", reason);
                    return;
                }
                GattConnectionEvent::Gatt { event } => {
                    let reply = match event {
                        GattEvent::Write(write) if write.handle() == rx_handle => {
                            let mut data = [0u8; MAX_BLE_PAYLOAD];
                            let mut len = 0usize;
                            let ok = write.with_data(|offset, bytes| {
                                if offset == 0 && bytes.len() <= MAX_BLE_PAYLOAD {
                                    data[..bytes.len()].copy_from_slice(bytes);
                                    len = bytes.len();
                                    true
                                } else {
                                    false
                                }
                            });
                            if ok {
                                // Waits for room: the central is not answered
                                // before the chunk is queued.
                                shared
                                    .rx
                                    .send(RxEvent::Data(Chunk::new(epoch, &data[..len])))
                                    .await;
                                write.accept_unprocessed()
                            } else {
                                write.reject(AttErrorCode::INVALID_ATTRIBUTE_VALUE_LENGTH)
                            }
                        }
                        other => other.accept(),
                    };
                    match reply {
                        Ok(reply) => reply.send().await,
                        Err(error) => warn!("BLE: GATT reply failed: {:?}", error),
                    }
                    refresh_link(shared, connection);
                }
                _ => refresh_link(shared, connection),
            }
        }
    };

    let notifier = async {
        loop {
            let chunk = shared.tx.receive().await;
            if chunk.epoch != epoch {
                continue;
            }
            if let Err(error) = tx.notify_raw(connection, chunk.bytes(), false).await {
                warn!("BLE: notification failed: {:?}", error);
                return;
            }
        }
    };

    if let Either::Second(()) = select(events, notifier).await {
        debug!("BLE: notifier stopped");
    }
}

async fn end_connection<const RX_DEPTH: usize, const TX_DEPTH: usize>(
    shared: &Shared<RX_DEPTH, TX_DEPTH>,
) {
    shared.update_link(|link| {
        link.info = None;
        link.state = BleState::Advertising;
    });
    while shared.tx.try_receive().is_ok() {}
    // Never dropped: the consumer needs this marker to reset its state.
    shared.rx.send(RxEvent::Disconnected).await;
    info!("BLE: central disconnected");
}
