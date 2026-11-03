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

//! The native control transport: ROHI control over localhost TCP.
//!
//! The simulator carries exactly the frames of the BLE link. [`TcpTransport`]
//! implements the portable [`BleTransport`] byte pipe, so the real
//! [`ControlBle`] serving loop, the real `rohi-control` [`Controller`] and the
//! protobuf messages are reused unchanged; only the carrier differs:
//!
//! ```text
//! rohi CLI -> TCP record -> TcpTransport -> ControlBle -> Controller -> UrbanDevice
//!
//! record = u16 little-endian length, then one BLE frame of `rohi_control_ble::frame`
//! ```
//!
//! A record is what one ATT write (request) or one notification (response)
//! is on BLE. One client is served at a time, like one BLE central; another
//! connection waits until the first one closes.
//!
//! [`ControlBle`]: rohi_sdk::ble::ControlBle
//! [`Controller`]: rohi_sdk::control::Controller

use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use altruist_urban::control::{Platform, ServicesSlot, UrbanDevice};
use altruist_urban::{config, identity, reboot};
use async_io::Async;
use embassy_executor::Spawner;
use log::{debug, info, warn};
use rohi_sdk::ble::ControlBle;
use rohi_sdk::control::{Controller, DeviceInfo, OpenPolicy};
use rohi_sdk::net::{
    BleConnectionInfo, BleError, BleName, BleProbe, BleState, BleTransport, MAX_BLE_PAYLOAD,
    NetworkHandle,
};
use static_cell::StaticCell;

use crate::io::{read_some, write_all};
use crate::network::NativeIp;

/// Firmware name and version.
const FIRMWARE: &str = concat!(env!("CARGO_PKG_NAME"), "-", env!("CARGO_PKG_VERSION"));

/// Board of the simulator.
const BOARD: &str = "urban-sim";

/// The state of the link, seen by the transport and its probe.
#[derive(Debug, Default)]
struct Link {
    state: BleState,
    name: Option<BleName>,
}

/// The view of the control link for the `ble` service.
///
/// The service reports the state of the control link of the device: `advertising`
/// while the simulator waits for a client, `connected` while one is served.
#[derive(Clone)]
pub struct LinkProbe(Arc<Mutex<Link>>);

impl BleProbe for LinkProbe {
    fn state(&self) -> BleState {
        self.0.lock().map(|link| link.state).unwrap_or_default()
    }

    fn max_payload(&self) -> usize {
        MAX_BLE_PAYLOAD
    }

    fn name(&self) -> Option<BleName> {
        self.0.lock().ok().and_then(|link| link.name.clone())
    }
}

/// The TCP carrier of the control frames.
pub struct TcpTransport {
    listener: Async<TcpListener>,
    connection: Option<Async<std::net::TcpStream>>,
    buffer: Vec<u8>,
    epoch: u32,
    link: Arc<Mutex<Link>>,
}

impl TcpTransport {
    /// A transport that accepts clients on `listener` (already bound).
    ///
    /// # Errors
    ///
    /// The listener cannot be registered with the reactor.
    pub fn new(listener: TcpListener, name: &str) -> std::io::Result<(Self, LinkProbe)> {
        let link = Arc::new(Mutex::new(Link {
            state: BleState::Advertising,
            name: BleName::new(name),
        }));
        let transport = Self {
            listener: Async::new(listener)?,
            connection: None,
            buffer: Vec::new(),
            epoch: 0,
            link: link.clone(),
        };
        Ok((transport, LinkProbe(link)))
    }

    fn set_state(&self, state: BleState) {
        if let Ok(mut link) = self.link.lock() {
            link.state = state;
        }
    }

    fn close(&mut self) {
        if self.connection.take().is_some() {
            debug!("control: client disconnected");
        }
        self.buffer.clear();
        self.set_state(BleState::Advertising);
    }

    /// The first complete record of the buffer, removed from it.
    fn take_record(&mut self) -> Option<Vec<u8>> {
        let length = usize::from(u16::from_le_bytes([
            *self.buffer.first()?,
            *self.buffer.get(1)?,
        ]));
        (self.buffer.len() >= 2 + length).then(|| {
            let record = self.buffer[2..2 + length].to_vec();
            self.buffer.drain(..2 + length);
            record
        })
    }
}

impl BleTransport for TcpTransport {
    async fn recv(&mut self, out: &mut [u8]) -> Result<usize, BleError> {
        loop {
            if self.connection.is_none() {
                let (stream, peer) = self.listener.accept().await.map_err(|e| {
                    warn!("control: accept failed: {e}");
                    BleError::DriverError
                })?;
                let _ = stream.get_ref().set_nodelay(true);
                self.epoch = self.epoch.wrapping_add(1);
                self.connection = Some(stream);
                self.set_state(BleState::Connected(BleConnectionInfo::default()));
                info!("control: client {peer} connected");
            }
            if let Some(record) = self.take_record() {
                if record.len() > out.len() {
                    self.close();
                    return Err(BleError::BufferTooSmall);
                }
                out[..record.len()].copy_from_slice(&record);
                return Ok(record.len());
            }
            let Some(stream) = &self.connection else {
                continue;
            };
            let mut chunk = [0u8; 512];
            match read_some(stream, &mut chunk).await {
                Ok(0) | Err(_) => {
                    self.close();
                    return Err(BleError::Disconnected);
                }
                Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
            }
        }
    }

    async fn send(&mut self, data: &[u8]) -> Result<(), BleError> {
        let Some(stream) = &self.connection else {
            return Err(BleError::Disconnected);
        };
        let Ok(length) = u16::try_from(data.len()) else {
            return Err(BleError::BufferTooSmall);
        };
        let mut record = Vec::with_capacity(2 + data.len());
        record.extend_from_slice(&length.to_le_bytes());
        record.extend_from_slice(data);
        if write_all(stream, &record).await.is_err() {
            self.close();
            return Err(BleError::Disconnected);
        }
        Ok(())
    }

    fn connection_info(&self) -> Option<BleConnectionInfo> {
        match self.link.lock().ok()?.state {
            BleState::Connected(info) => Some(info),
            _ => None,
        }
    }

    fn max_payload(&self) -> usize {
        MAX_BLE_PAYLOAD
    }

    fn state(&self) -> BleState {
        self.link.lock().map(|link| link.state).unwrap_or_default()
    }

    fn connection_epoch(&self) -> u32 {
        self.epoch
    }
}

/// The control backend of the simulated Altruist Urban.
pub type SimDevice = UrbanDevice<NativeIp, LinkProbe>;

static SERVICES: ServicesSlot<NativeIp, LinkProbe> = StaticCell::new();

/// The device over the runtime handles of the firmware.
pub fn device(network: Option<NetworkHandle<'static, NativeIp>>, link: LinkProbe) -> SimDevice {
    altruist_urban::control::device(
        Platform {
            info: DeviceInfo {
                firmware: FIRMWARE,
                board: BOARD,
            },
            services: &SERVICES,
            network,
            heap: None,
            ble: link,
            reboot: reboot::request,
        },
        config::handle(),
        identity::start(config::handle()),
    )
}

/// Serves control requests received over TCP.
///
/// As on BLE there is no link security policy yet: [`OpenPolicy`] allows
/// everything, including identity provisioning and erase. The listener is
/// bound to the loopback address unless `--bind` says otherwise.
#[embassy_executor::task]
pub async fn control_task(transport: TcpTransport, device: SimDevice) {
    let controller = Controller::new(device, OpenPolicy);
    let mut control = ControlBle::new(transport, &controller);
    control.run().await
}

/// Starts the control task on `listener`.
///
/// # Errors
///
/// The listener cannot be registered with the reactor.
pub fn start(
    spawner: Spawner,
    listener: TcpListener,
    name: &str,
    network: Option<NetworkHandle<'static, NativeIp>>,
) -> std::io::Result<()> {
    let (transport, link) = TcpTransport::new(listener, name)?;
    spawner.spawn(control_task(transport, device(network, link)).unwrap());
    Ok(())
}
