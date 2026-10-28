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
//! Signed envelope construction and the immutable frame.

use core::fmt;

use heapless::Vec;
use rohi_connectivity_proto::crypto_v1::SignedEnvelope;
use rohi_connectivity_proto::limits::{ED25519_KEY_LEN, MAX_ENVELOPE_LEN, MAX_MESSAGE_LEN};
use rohi_connectivity_proto::{encode, signing_preimage};
use rohi_identity::{IdentityError, PublicKey, Signature, Signer};

use crate::clock::{NONCE_LEN, NonceSource};

/// A signed telemetry frame: the serialized `crypto.v1.SignedEnvelope`.
///
/// The bytes are created once and never change. Retries send exactly these
/// bytes; the nonce, the timestamp and the signature are never regenerated and
/// the envelope is never re-encoded from decoded fields.
#[derive(Clone, PartialEq, Eq)]
pub struct EnvelopeFrame<const N: usize = MAX_ENVELOPE_LEN> {
    bytes: Vec<u8, N>,
}

impl<const N: usize> EnvelopeFrame<N> {
    /// The serialized envelope.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Length of the frame in bytes.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Always `false`: a frame holds a non-empty envelope.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl<const N: usize> fmt::Debug for EnvelopeFrame<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnvelopeFrame")
            .field("len", &self.bytes.len())
            .finish()
    }
}

/// The signer cannot sign.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignError {
    /// The device has no identity yet.
    NotProvisioned,
    /// Signing failed.
    Failed,
}

impl From<IdentityError> for SignError {
    fn from(error: IdentityError) -> Self {
        match error {
            IdentityError::NotProvisioned => Self::NotProvisioned,
            _ => Self::Failed,
        }
    }
}

/// The device identity as seen by the telemetry service.
///
/// The identity may appear (provisioning) or disappear (erase) while the
/// device runs. Private key bytes never cross this boundary: only the public
/// key and signatures do. Every [`rohi_identity::Signer`] returning
/// [`IdentityError`] is a `FrameSigner`.
pub trait FrameSigner {
    /// Public key of the identity: the `sensor_id` of the envelope.
    fn sensor_id(&self) -> Result<PublicKey, SignError>;

    /// Ed25519 signature over exactly `message`.
    fn sign_payload(&self, message: &[u8]) -> Result<Signature, SignError>;
}

impl<S: Signer<Error = IdentityError>> FrameSigner for S {
    fn sensor_id(&self) -> Result<PublicKey, SignError> {
        Ok(self.public_key())
    }

    fn sign_payload(&self, message: &[u8]) -> Result<Signature, SignError> {
        Signer::sign(self, message).map_err(SignError::from)
    }
}

/// A frame could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildError {
    /// The device has no identity.
    NotProvisioned,
    /// Signing failed.
    Signing,
    /// No entropy for the nonce.
    Nonce,
    /// The message or the envelope exceeds its bound.
    TooLarge,
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotProvisioned => "device identity not provisioned",
            Self::Signing => "signing failed",
            Self::Nonce => "no entropy for the nonce",
            Self::TooLarge => "telemetry frame too large",
        })
    }
}

impl core::error::Error for BuildError {}

/// Builds [`EnvelopeFrame`]s: nonce, signature over
/// `sensor_id || nonce || message`, one envelope serialization.
pub struct EnvelopeBuilder<'a, S, N> {
    signer: &'a S,
    nonce: &'a mut N,
}

impl<'a, S: FrameSigner, N: NonceSource> EnvelopeBuilder<'a, S, N> {
    /// A builder signing with `signer` and drawing nonces from `nonce`.
    pub fn new(signer: &'a S, nonce: &'a mut N) -> Self {
        Self { signer, nonce }
    }

    /// Signs the serialized `core.v1.Message` `message` and wraps it.
    ///
    /// `message` is used byte-for-byte; it is not decoded, modified or
    /// re-encoded. A new nonce is drawn for every call.
    pub fn sign<const FRAME: usize>(
        &mut self,
        message: &[u8],
    ) -> Result<EnvelopeFrame<FRAME>, BuildError> {
        if message.len() > MAX_MESSAGE_LEN {
            return Err(BuildError::TooLarge);
        }
        let sensor_id = self.signer.sensor_id().map_err(|error| match error {
            SignError::NotProvisioned => BuildError::NotProvisioned,
            SignError::Failed => BuildError::Signing,
        })?;
        let mut nonce = [0u8; NONCE_LEN];
        self.nonce
            .fill_nonce(&mut nonce)
            .map_err(|_| BuildError::Nonce)?;

        let mut preimage = [0u8; ED25519_KEY_LEN + NONCE_LEN + MAX_MESSAGE_LEN];
        let len = signing_preimage(sensor_id.as_bytes(), &nonce, message, &mut preimage)
            .map_err(|_| BuildError::TooLarge)?;
        let signature =
            self.signer
                .sign_payload(&preimage[..len])
                .map_err(|error| match error {
                    SignError::NotProvisioned => BuildError::NotProvisioned,
                    SignError::Failed => BuildError::Signing,
                })?;

        let mut envelope = SignedEnvelope::default();
        let too_large = |_| BuildError::TooLarge;
        envelope
            .sensor_id
            .extend_from_slice(sensor_id.as_bytes())
            .map_err(too_large)?;
        envelope
            .nonce
            .extend_from_slice(&nonce)
            .map_err(too_large)?;
        envelope
            .message
            .extend_from_slice(message)
            .map_err(too_large)?;
        envelope
            .signature
            .extend_from_slice(signature.as_bytes())
            .map_err(too_large)?;

        let mut bytes = Vec::new();
        bytes
            .resize_default(FRAME)
            .map_err(|_| BuildError::TooLarge)?;
        let len = encode(&envelope, &mut bytes).map_err(|_| BuildError::TooLarge)?;
        bytes.truncate(len);
        Ok(EnvelopeFrame { bytes })
    }
}
