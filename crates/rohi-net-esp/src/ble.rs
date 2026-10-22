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

use core::cell::Cell;

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
    BleConfig, BleConnectionInfo, BleError, BleState, BleTransport, DEFAULT_BLE_PAYLOAD,
    MAX_BLE_PAYLOAD,
};
use trouble_host::prelude::*;

use crate::wireless::WirelessError;

/// Chunks buffered from the central before the writer is held back.
const RX_DEPTH: usize = 4;
/// Chunks buffered for notification.
const TX_DEPTH: usize = 4;
/// ATT header of a write or notification.
const ATT_OVERHEAD: u16 = 3;

const CONNECTIONS_MAX: usize = 1;
/// Signalling, ATT.
const L2CAP_CHANNELS_MAX: usize = 2;
/// GAP, GATT and the ROHI service with their characteristics and descriptors.
const ATTRIBUTES_MAX: usize = 24;
/// HCI command slots of the external controller.
const CONTROLLER_SLOTS: usize = 4;

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

static RX: Channel<CriticalSectionRawMutex, RxEvent, RX_DEPTH> = Channel::new();
static TX: Channel<CriticalSectionRawMutex, Chunk, TX_DEPTH> = Channel::new();
static LINK: Mutex<CriticalSectionRawMutex, Cell<Link>> = Mutex::new(Cell::new(Link {
    state: BleState::Disabled,
    epoch: 0,
    payload: DEFAULT_BLE_PAYLOAD,
    info: None,
}));
static TAKEN: Mutex<CriticalSectionRawMutex, Cell<bool>> = Mutex::new(Cell::new(false));

fn link() -> Link {
    LINK.lock(Cell::get)
}

fn update_link(f: impl FnOnce(&mut Link)) {
    LINK.lock(|cell| {
        let mut value = cell.get();
        f(&mut value);
        cell.set(value);
    });
}

/// The connected central as a [`BleTransport`].
pub struct EspBle {
    _private: (),
}

impl BleTransport for EspBle {
    async fn recv(&mut self, out: &mut [u8]) -> Result<usize, BleError> {
        match RX.receive().await {
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

    async fn send(&mut self, data: &[u8]) -> Result<(), BleError> {
        let current = link();
        if current.info.is_none() {
            return Err(BleError::Disconnected);
        }
        if data.len() > current.payload {
            return Err(BleError::BufferTooSmall);
        }
        TX.send(Chunk::new(current.epoch, data)).await;
        Ok(())
    }

    fn connection_info(&self) -> Option<BleConnectionInfo> {
        link().info
    }

    fn max_payload(&self) -> usize {
        link().payload
    }

    fn state(&self) -> BleState {
        link().state
    }

    fn connection_epoch(&self) -> u32 {
        link().epoch
    }
}

/// Drives the BLE controller, host stack, advertising and GATT server.
/// Run it on its own task: `runner.run().await`.
pub struct BleRunner {
    connector: BleConnector<'static>,
    config: BleConfig,
}

pub(crate) fn new(
    bt: esp_hal::peripherals::BT<'static>,
    config: BleConfig,
) -> Result<(EspBle, BleRunner), WirelessError> {
    if TAKEN.lock(|taken| taken.replace(true)) {
        return Err(WirelessError::AlreadyTaken);
    }
    let connector = BleConnector::new(bt, Default::default()).map_err(|error| {
        warn!("BLE controller init failed: {:?}", error);
        WirelessError::BleInit
    })?;
    update_link(|link| link.state = BleState::Advertising);
    Ok((EspBle { _private: () }, BleRunner { connector, config }))
}

/// Static random address derived from the factory MAC.
fn address() -> Address {
    let mut bytes: [u8; 6] = base_mac_address().as_bytes().try_into().unwrap_or([0; 6]);
    // The two most significant bits of a static random address are 1.
    bytes[5] |= 0xC0;
    Address::random(bytes)
}

impl BleRunner {
    /// Runs the BLE stack forever. Errors restart advertising; they never
    /// stop the rest of the firmware.
    pub async fn run(self) -> ! {
        let controller: ExternalController<_, CONTROLLER_SLOTS> =
            ExternalController::new(self.connector);
        let mut resources: HostResources<
            _,
            DefaultPacketPool,
            CONNECTIONS_MAX,
            L2CAP_CHANNELS_MAX,
        > = HostResources::new();
        let stack = trouble_host::new(controller, &mut resources)
            .set_random_address(address())
            .build();
        let mut peripheral = stack.peripheral();
        let mut runner = stack.runner();

        let mut rx_store = [0u8; MAX_BLE_PAYLOAD];
        let mut tx_store = [0u8; MAX_BLE_PAYLOAD];
        let mut table: AttributeTable<'_, CriticalSectionRawMutex, ATTRIBUTES_MAX> =
            AttributeTable::new();
        if GapConfig::default(self.config.name)
            .build(&mut table)
            .is_err()
        {
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
            ATTRIBUTES_MAX,
            CONNECTIONS_MAX,
        > = AttributeServer::new(table);

        let host = async {
            loop {
                if let Err(error) = runner.run().await {
                    warn!("BLE host stopped: {:?}", error);
                    update_link(|link| link.state = BleState::Error(BleError::DriverError));
                }
            }
        };

        let serve = async {
            loop {
                match advertise(&self.config, &mut peripheral, &server).await {
                    Ok(connection) => {
                        serve_connection(&connection, rx_handle.handle, &tx_handle).await;
                        end_connection().await;
                    }
                    Err(error) => {
                        warn!("BLE advertising failed: {:?}", error);
                        update_link(|link| link.state = BleState::Error(BleError::DriverError));
                        embassy_time::Timer::after_secs(1).await;
                    }
                }
            }
        };

        join(host, serve).await;
        unreachable!("BLE tasks never end")
    }
}

async fn advertise<'values, 'server, C: Controller>(
    config: &BleConfig,
    peripheral: &mut Peripheral<'values, C, DefaultPacketPool>,
    server: &'server AttributeServer<
        'values,
        CriticalSectionRawMutex,
        DefaultPacketPool,
        ATTRIBUTES_MAX,
        CONNECTIONS_MAX,
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
    let name = config.name.as_bytes();
    let name = &name[..name.len().min(29)];
    let scan_len = AdStructure::encode_slice(
        &[AdStructure::CompleteLocalName(name)],
        &mut scan_response[..],
    )?;

    update_link(|link| {
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

fn refresh_link(connection: &GattConnection<'_, '_, DefaultPacketPool>) {
    let info = connection_info(connection);
    let mtu = connection.raw().att_mtu().saturating_sub(ATT_OVERHEAD);
    let payload = usize::from(mtu).clamp(DEFAULT_BLE_PAYLOAD, MAX_BLE_PAYLOAD);
    update_link(|link| {
        link.info = Some(info);
        link.payload = payload;
        link.state = BleState::Connected(info);
    });
}

/// Serves one connection until it ends: receives writes and notifies
/// responses.
async fn serve_connection(
    connection: &GattConnection<'_, '_, DefaultPacketPool>,
    rx_handle: u16,
    tx: &Characteristic<Vec<u8, MAX_BLE_PAYLOAD>>,
) {
    // A new connection starts clean: nothing of the previous one survives.
    while RX.try_receive().is_ok() {}
    while TX.try_receive().is_ok() {}
    update_link(|link| {
        link.epoch = link.epoch.wrapping_add(1);
        link.payload = DEFAULT_BLE_PAYLOAD;
    });
    refresh_link(connection);
    info!("BLE: central connected");
    let epoch = link().epoch;

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
                                RX.send(RxEvent::Data(Chunk::new(epoch, &data[..len])))
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
                    refresh_link(connection);
                }
                _ => refresh_link(connection),
            }
        }
    };

    let notifier = async {
        loop {
            let chunk = TX.receive().await;
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

async fn end_connection() {
    update_link(|link| {
        link.info = None;
        link.state = BleState::Advertising;
    });
    while TX.try_receive().is_ok() {}
    // Never dropped: the consumer needs this marker to reset its state.
    RX.send(RxEvent::Disconnected).await;
    info!("BLE: central disconnected");
}
