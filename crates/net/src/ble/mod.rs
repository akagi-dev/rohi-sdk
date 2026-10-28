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
//! Portable BLE peripheral transport.
//!
//! A BLE backend (for example `rohi-net-esp`) exposes one connected central
//! as a bounded byte pipe through [`BleTransport`]. This layer only moves
//! bytes and reports link properties; framing and protocol meaning belong to
//! the layers above. No platform (controller, host stack) type appears in
//! this API.

use core::fmt;

mod config;
mod managed;
mod service;

pub use config::{BleName, BleOverrides, BleSettings, MAX_BLE_NAME_LEN};
pub use service::{BleProbe, BleService, STATES, issue, state, status_field};

/// Largest ATT payload a BLE transport ever has to carry (MTU 247 minus the
/// 3-byte ATT header). Fragmenting layers must never exceed
/// [`BleTransport::max_payload`], which is at most this value.
pub const MAX_BLE_PAYLOAD: usize = 244;

/// Payload size of an ATT write/notification at the default MTU (23).
pub const DEFAULT_BLE_PAYLOAD: usize = 20;

/// GATT layout of the byte pipe a BLE backend exposes: one service with a
/// write characteristic (central to peripheral) and a notify characteristic
/// (peripheral to central). UUIDs are 128-bit values.
///
/// The backend only creates the attributes; their meaning belongs to the
/// protocol layer that supplies the UUIDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BleGattSpec {
    /// Primary service.
    pub service: u128,
    /// Characteristic the central writes to; every write is one received chunk.
    pub rx: u128,
    /// Characteristic the peripheral notifies; every notification is one sent chunk.
    pub tx: u128,
}

/// BLE peripheral configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BleConfig {
    /// Prefix of the default advertised and GAP device name, used while
    /// [`name`](Self::name) is `None`. The backend appends `-` and the last
    /// two bytes of the device MAC in upper-case hex, for example
    /// `ALTRUIST-URBAN` becomes `ALTRUIST-URBAN-A1B2`. Long prefixes are cut to
    /// fit the advertising data.
    pub name_prefix: &'static str,
    /// The configured name, advertised as it is. Replaces the default name.
    pub name: Option<BleName>,
    /// GATT layout of the byte pipe.
    pub gatt: BleGattSpec,
}

impl BleConfig {
    /// The same configuration with the name of `settings`.
    pub fn with_settings(self, settings: &BleSettings) -> Self {
        Self {
            name: settings.name.clone(),
            ..self
        }
    }
}

/// Security properties of the current link.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct BleConnectionInfo {
    /// The link is encrypted.
    pub encrypted: bool,
    /// The link is encrypted with an authenticated key (MITM protected).
    pub authenticated: bool,
}

/// Platform-independent BLE error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BleError {
    /// No central is connected, or it disconnected. Returned once from
    /// [`BleTransport::recv`] when a connection ends; the next call waits for
    /// a new connection.
    Disconnected,
    /// The buffer is smaller than the received chunk or the outgoing chunk
    /// exceeds [`BleTransport::max_payload`].
    BufferTooSmall,
    /// The outgoing queue is full or the central is not consuming data.
    Busy,
    /// The platform driver failed.
    DriverError,
}

impl fmt::Display for BleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Disconnected => "BLE central disconnected",
            Self::BufferTooSmall => "BLE buffer too small",
            Self::Busy => "BLE transport is busy",
            Self::DriverError => "BLE driver error",
        })
    }
}

/// Published state of the BLE peripheral.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BleState {
    /// BLE is not running.
    #[default]
    Disabled,
    /// Advertising and waiting for a central.
    Advertising,
    /// A central is connected.
    Connected(BleConnectionInfo),
    /// The radio or host stack failed; the backend retries.
    Error(BleError),
}

/// A connected central seen as a bounded byte pipe.
///
/// One chunk corresponds to one ATT write (`recv`) or one notification
/// (`send`). Chunks are never merged or split by the transport. Futures must
/// be cancel-safe.
#[allow(async_fn_in_trait)]
pub trait BleTransport {
    /// Waits for the next chunk written by the central and copies it into
    /// `out`, returning its length.
    ///
    /// Pends while no central is connected. When the connection ends it
    /// returns [`BleError::Disconnected`] exactly once; the caller must then
    /// drop all partial state tied to that connection.
    async fn recv(&mut self, out: &mut [u8]) -> Result<usize, BleError>;

    /// Sends one chunk to the central; `data.len()` must not exceed
    /// [`BleTransport::max_payload`].
    async fn send(&mut self, data: &[u8]) -> Result<(), BleError>;

    /// Security properties of the current connection, `None` when no central
    /// is connected.
    fn connection_info(&self) -> Option<BleConnectionInfo>;

    /// Largest chunk the current link carries, between
    /// [`DEFAULT_BLE_PAYLOAD`] and [`MAX_BLE_PAYLOAD`]. Changes with the
    /// negotiated MTU.
    fn max_payload(&self) -> usize;

    /// Current state of the peripheral.
    fn state(&self) -> BleState;

    /// Number of the current connection; changes with every new connection.
    ///
    /// Lets upper layers tell whether a response still belongs to the central
    /// that sent the request after an `await` during which the link may have
    /// dropped and been re-established.
    fn connection_epoch(&self) -> u32;
}
