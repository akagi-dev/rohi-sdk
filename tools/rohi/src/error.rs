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
//! Errors of the `rohi` library.

use rohi_control_ble::frame::FrameError;
use rohi_control_proto::{DecodeError, EncodeError};
use thiserror::Error;

use rohi_identity::PublicKey;

use crate::model::ErrorCode;

/// Everything that can go wrong talking to a device.
#[derive(Debug, Error)]
pub enum Error {
    /// The BLE link failed (adapter, discovery, connection, GATT).
    #[error("BLE transport: {0}")]
    Transport(String),
    /// No device was found.
    #[error("no device found")]
    DeviceNotFound,
    /// The device did not answer in time.
    #[error("timed out waiting for the device")]
    Timeout,
    /// The device sent something that is not a valid response.
    #[error("protocol violation: {0}")]
    Protocol(String),
    /// A frame could not be built or was rejected.
    #[error("framing: {0}")]
    Frame(FrameError),
    /// The request or response does not fit the protocol limits.
    #[error("message too large")]
    TooLarge,
    /// The device answered with a typed protocol error.
    #[error("device error: {0}")]
    Device(ErrorCode),
    /// The action ran and was refused for a stable reason, for example
    /// `already_provisioned`.
    #[error("rejected by the device: {0}")]
    Rejected(String),
    /// A value given by the caller is invalid.
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// The device reported a public key other than the locally derived one, so the
    /// provisioned identity is not the intended one.
    #[error(
        "public key mismatch: derived {expected}, the device reports {reported}; \
         erase the identity and provision again"
    )]
    PublicKeyMismatch {
        /// Derived on the host.
        expected: PublicKey,
        /// Returned by the device.
        reported: PublicKey,
    },
}

impl From<FrameError> for Error {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

impl From<EncodeError> for Error {
    fn from(_: EncodeError) -> Self {
        Self::TooLarge
    }
}

impl From<DecodeError> for Error {
    fn from(error: DecodeError) -> Self {
        Self::Protocol(error.to_string())
    }
}

impl From<btleplug::Error> for Error {
    fn from(error: btleplug::Error) -> Self {
        Self::Transport(error.to_string())
    }
}

/// Result of library calls.
pub type Result<T, E = Error> = core::result::Result<T, E>;
