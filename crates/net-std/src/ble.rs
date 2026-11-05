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

//! The Bluetooth LE peripheral of a Linux host, through BlueZ.
//!
//! Available with the `ble` feature on Linux. [`start`] returns a
//! [`BluezBle`], the [`BleTransport`] a control link
//! ([`rohi_control_ble::ControlBle`]) runs on, and a [`BluezProbe`], the view of
//! the same peripheral for the `ble` service. The GATT layout is the one of the
//! firmware: [`BleConfig::gatt`] names a service with a write characteristic
//! (`rx`) and a notify characteristic (`tx`); framing and the protocol are the
//! layers above, exactly as on the ESP.
//!
//! ```text
//! mobile (central) --write--> rx --> BlueZ (D-Bus) --> Peripheral --> BluezBle::recv
//! mobile (central) <-notify-- tx <-- BlueZ (D-Bus) <-- Peripheral <-- BluezBle::send
//! ```
//!
//! BlueZ is reached over D-Bus by the `bluer` crate, on the reactor of
//! [`runtime`](crate::runtime); the transport itself is polled by the caller's
//! executor and only exchanges bounded queues with it.
//!
//! # Link life cycle
//!
//! The peripheral supervises BlueZ in the background. If BlueZ, the D-Bus socket
//! or the adapter is missing or fails, the state is
//! [`BleState::Error`] and bringing the peripheral up is retried every few
//! seconds; nothing else of the application is affected. When it is up, the
//! device advertises the service UUID with the name of the
//! [`BleConfig`] (or `<PREFIX>-<last two bytes of the adapter address>`).
//!
//! BlueZ does not report an ATT connection to a GATT application. A link is
//! therefore considered connected when a central subscribes to `tx` or writes
//! to `rx`, and disconnected when its notification session ends (BlueZ ends it
//! when the central disconnects). The advertisement is registered again after
//! every disconnect so that the central can reconnect. One central is served at
//! a time; a second central that subscribes while one is connected takes over
//! its notifications only after the first one is gone.
//!
//! # Limitations
//!
//! * Not verified on real hardware or with a phone by the automated tests: the
//!   host tests drive the transport through the same bridge BlueZ uses, with
//!   `rohi-control-ble` on top, but never talk to BlueZ.
//! * No pairing, encryption or authentication: the reported link security is
//!   always the default one, so `rohi_control::OpenPolicy` applies
//!   unless the application chooses another policy.
//! * The payload is the negotiated ATT MTU minus 3 bytes, at most
//!   [`MAX_BLE_PAYLOAD`], and 20 bytes until the central wrote its first request.
//! * BlueZ needs a Bluetooth adapter with LE peripheral support, a running
//!   `bluetoothd` and permission to use the system bus (see the std guide).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use log::{debug, info, warn};
use rohi_net::{
    BleConfig, BleConnectionInfo, BleError, BleName, BleProbe, BleState, BleTransport,
    DEFAULT_BLE_PAYLOAD, MAX_BLE_PAYLOAD,
};
use tokio::sync::{Notify, mpsc};

use crate::runtime::handle;

/// How long the supervisor waits before it brings the peripheral up again.
const RETRY: Duration = Duration::from_secs(5);

/// Chunks queued in each direction.
const QUEUE: usize = 8;

/// ATT header of a notification and of a write.
const ATT_HEADER: usize = 3;

#[derive(Debug)]
struct Link {
    state: BleState,
    name: Option<BleName>,
    payload: usize,
    epoch: u32,
}

#[derive(Debug)]
struct Shared(Mutex<Link>);

impl Shared {
    fn with<T>(&self, f: impl FnOnce(&mut Link) -> T) -> T {
        let mut link = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut link)
    }
}

enum Inbound {
    Chunk(Vec<u8>),
    Disconnected,
}

/// The view of the peripheral for the `ble` service. Cloning it gives another
/// view of the same peripheral.
#[derive(Clone, Debug)]
pub struct BluezProbe(Arc<Shared>);

impl BleProbe for BluezProbe {
    fn state(&self) -> BleState {
        self.0.with(|link| link.state)
    }

    fn max_payload(&self) -> usize {
        self.0.with(|link| link.payload)
    }

    fn name(&self) -> Option<BleName> {
        self.0.with(|link| link.name.clone())
    }
}

/// The BLE byte pipe to one central, a [`BleTransport`].
///
/// Dropping it ends the use of the link; the supervisor keeps running for the
/// life of the process.
#[derive(Debug)]
pub struct BluezBle {
    inbox: mpsc::Receiver<Inbound>,
    outbox: mpsc::Sender<Vec<u8>>,
    shared: Arc<Shared>,
}

impl BluezBle {
    /// A view of this peripheral for the `ble` service.
    pub fn probe(&self) -> BluezProbe {
        BluezProbe(self.shared.clone())
    }
}

impl BleTransport for BluezBle {
    async fn recv(&mut self, out: &mut [u8]) -> Result<usize, BleError> {
        match self.inbox.recv().await {
            Some(Inbound::Chunk(chunk)) => {
                if chunk.len() > out.len() {
                    return Err(BleError::BufferTooSmall);
                }
                out[..chunk.len()].copy_from_slice(&chunk);
                Ok(chunk.len())
            }
            Some(Inbound::Disconnected) => Err(BleError::Disconnected),
            // The supervisor is gone: nothing will ever arrive.
            None => core::future::pending().await,
        }
    }

    async fn send(&mut self, data: &[u8]) -> Result<(), BleError> {
        if data.len() > self.max_payload() {
            return Err(BleError::BufferTooSmall);
        }
        if self.connection_info().is_none() {
            return Err(BleError::Disconnected);
        }
        self.outbox
            .send(data.to_vec())
            .await
            .map_err(|_| BleError::DriverError)
    }

    fn connection_info(&self) -> Option<BleConnectionInfo> {
        match self.state() {
            BleState::Connected(info) => Some(info),
            _ => None,
        }
    }

    fn max_payload(&self) -> usize {
        self.shared.with(|link| link.payload)
    }

    fn state(&self) -> BleState {
        self.shared.with(|link| link.state)
    }

    fn connection_epoch(&self) -> u32 {
        self.shared.with(|link| link.epoch)
    }
}

/// The side of the bridge that BlueZ callbacks use.
#[derive(Clone)]
struct Peripheral {
    shared: Arc<Shared>,
    inbox: mpsc::Sender<Inbound>,
    outbox: Arc<tokio::sync::Mutex<mpsc::Receiver<Vec<u8>>>>,
    ended: Arc<Notify>,
}

fn bridge() -> (BluezBle, Peripheral) {
    let shared = Arc::new(Shared(Mutex::new(Link {
        state: BleState::Disabled,
        name: None,
        payload: DEFAULT_BLE_PAYLOAD,
        epoch: 0,
    })));
    let (inbox_tx, inbox_rx) = mpsc::channel(QUEUE);
    let (outbox_tx, outbox_rx) = mpsc::channel(QUEUE);
    let transport = BluezBle {
        inbox: inbox_rx,
        outbox: outbox_tx,
        shared: shared.clone(),
    };
    let peripheral = Peripheral {
        shared,
        inbox: inbox_tx,
        outbox: Arc::new(tokio::sync::Mutex::new(outbox_rx)),
        ended: Arc::new(Notify::new()),
    };
    (transport, peripheral)
}

impl Peripheral {
    fn set_state(&self, state: BleState) {
        self.shared.with(|link| link.state = state);
    }

    fn set_name(&self, name: Option<BleName>) {
        self.shared.with(|link| link.name = name);
    }

    fn is_connected(&self) -> bool {
        self.shared
            .with(|link| matches!(link.state, BleState::Connected(_)))
    }

    /// A central is using the link. Starts a new connection unless one is
    /// already tracked.
    fn connected(&self) {
        self.shared.with(|link| {
            if !matches!(link.state, BleState::Connected(_)) {
                link.epoch = link.epoch.wrapping_add(1);
                link.state = BleState::Connected(BleConnectionInfo::default());
                link.payload = DEFAULT_BLE_PAYLOAD;
            }
        });
    }

    /// The ATT MTU negotiated with the central.
    fn mtu(&self, mtu: u16) {
        let payload = usize::from(mtu)
            .saturating_sub(ATT_HEADER)
            .clamp(DEFAULT_BLE_PAYLOAD, MAX_BLE_PAYLOAD);
        self.shared.with(|link| link.payload = payload);
    }

    /// A chunk written by the central.
    async fn received(&self, chunk: Vec<u8>) {
        let _ = self.inbox.send(Inbound::Chunk(chunk)).await;
    }

    /// The connection ended: tells the transport once and the supervisor.
    async fn disconnected(&self) {
        let was = self.shared.with(|link| {
            let was = matches!(link.state, BleState::Connected(_));
            if was {
                link.state = BleState::Advertising;
                link.payload = DEFAULT_BLE_PAYLOAD;
            }
            was
        });
        if was {
            let _ = self.inbox.send(Inbound::Disconnected).await;
            self.ended.notify_one();
        }
    }
}

/// The name the peripheral advertises: the configured one, or the prefix and
/// the last two bytes of the adapter address.
fn advertised_name(config: &BleConfig, address: [u8; 6]) -> Option<BleName> {
    if let Some(name) = &config.name {
        return Some(name.clone());
    }
    let suffix = format!("-{:02X}{:02X}", address[4], address[5]);
    let prefix_len = rohi_net::MAX_BLE_NAME_LEN.saturating_sub(suffix.len());
    let mut prefix = config.name_prefix;
    while prefix.len() > prefix_len {
        prefix = &prefix[..prefix.floor_char_boundary(prefix.len() - 1)];
    }
    BleName::new(&format!("{prefix}{suffix}"))
}

/// Starts the peripheral on `adapter` (the default adapter of BlueZ when
/// `None`) and returns its transport.
///
/// Returns at once: bringing BlueZ up happens in the background, see the
/// [module documentation](self). `config` is read once.
pub fn start(config: BleConfig, adapter: Option<String>) -> BluezBle {
    let (transport, peripheral) = bridge();
    handle().spawn(supervise(config, adapter, peripheral));
    transport
}

async fn supervise(config: BleConfig, adapter: Option<String>, peripheral: Peripheral) {
    let mut last = String::new();
    loop {
        let error = match serve(&config, adapter.as_deref(), &peripheral).await {
            Ok(()) => continue,
            Err(error) => error,
        };
        peripheral.set_state(BleState::Error(BleError::DriverError));
        if last != error {
            warn!(
                "BLE unavailable: {error}; retrying every {} s",
                RETRY.as_secs()
            );
            last = error;
        }
        tokio::time::sleep(RETRY).await;
    }
}

/// Brings the peripheral up and serves it until BlueZ fails.
async fn serve(
    config: &BleConfig,
    adapter: Option<&str>,
    peripheral: &Peripheral,
) -> Result<(), String> {
    use bluer::Uuid;
    use bluer::adv::{Advertisement, Type};
    use bluer::gatt::local::{
        Application, Characteristic, CharacteristicNotify, CharacteristicNotifyMethod,
        CharacteristicWrite, CharacteristicWriteMethod, Service,
    };

    let describe = |context: &'static str| move |e: bluer::Error| format!("{context}: {e}");
    let session = bluer::Session::new()
        .await
        .map_err(describe("cannot reach BlueZ over D-Bus"))?;
    let adapter = match adapter {
        Some(name) => session.adapter(name).map_err(describe("no such adapter"))?,
        None => session
            .default_adapter()
            .await
            .map_err(describe("no Bluetooth adapter"))?,
    };
    adapter
        .set_powered(true)
        .await
        .map_err(describe("cannot power the adapter on"))?;
    let address = adapter
        .address()
        .await
        .map_err(describe("cannot read the adapter address"))?;
    let name = advertised_name(config, address.0);
    peripheral.set_name(name.clone());

    let write = {
        let peripheral = peripheral.clone();
        CharacteristicWrite {
            write: true,
            write_without_response: true,
            method: CharacteristicWriteMethod::Fun(Box::new(move |value, request| {
                let peripheral = peripheral.clone();
                Box::pin(async move {
                    peripheral.connected();
                    peripheral.mtu(request.mtu);
                    peripheral.received(value).await;
                    Ok(())
                })
            })),
            ..Default::default()
        }
    };
    let notify = {
        let peripheral = peripheral.clone();
        CharacteristicNotify {
            notify: true,
            method: CharacteristicNotifyMethod::Fun(Box::new(move |notifier| {
                Box::pin(notifications(peripheral.clone(), notifier))
            })),
            ..Default::default()
        }
    };
    let application = Application {
        services: vec![Service {
            uuid: Uuid::from_u128(config.gatt.service),
            primary: true,
            characteristics: vec![
                Characteristic {
                    uuid: Uuid::from_u128(config.gatt.rx),
                    write: Some(write),
                    ..Default::default()
                },
                Characteristic {
                    uuid: Uuid::from_u128(config.gatt.tx),
                    notify: Some(notify),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }],
        ..Default::default()
    };
    let _application = adapter
        .serve_gatt_application(application)
        .await
        .map_err(describe("cannot publish the GATT service"))?;

    info!(
        "BLE peripheral on {} as {}",
        adapter.name(),
        name.as_ref().map_or("(unnamed)", BleName::as_str)
    );
    loop {
        let advertisement = Advertisement {
            advertisement_type: Type::Peripheral,
            service_uuids: [Uuid::from_u128(config.gatt.service)].into(),
            discoverable: Some(true),
            local_name: name.as_ref().map(|name| name.as_str().to_owned()),
            ..Default::default()
        };
        let _advertising = adapter
            .advertise(advertisement)
            .await
            .map_err(describe("cannot advertise"))?;
        if !peripheral.is_connected() {
            peripheral.set_state(BleState::Advertising);
        }
        debug!("BLE advertising");
        // The advertisement is registered again after every connection.
        peripheral.ended.notified().await;
    }
}

/// A notification session of a central: sends what the transport queues until
/// the central stops it.
async fn notifications(
    peripheral: Peripheral,
    mut notifier: bluer::gatt::local::CharacteristicNotifier,
) {
    peripheral.connected();
    let mut outbox = peripheral.outbox.lock().await;
    loop {
        tokio::select! {
            () = notifier.stopped() => break,
            chunk = outbox.recv() => {
                let Some(chunk) = chunk else { break };
                if notifier.notify(chunk).await.is_err() {
                    break;
                }
            }
        }
    }
    drop(outbox);
    peripheral.disconnected().await;
}

#[cfg(test)]
mod tests;
