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
//! The packet format: sections to an encrypted, signed envelope.

use core::fmt;

use rohi_identity::{IdentityError, KeyAgreement, PUBLIC_KEY_LEN, PublicKey, Signature, Signer};
use rohi_telemetry::{
    PacketFormat, PacketFormatId, PacketInfo, PreparedSections, RecipientAddress, SealError,
    Selection, SignedPacket,
};
use rohi_time::{Clock, MonotonicTimestamp};

use crate::crypto::{self, MAX_CIPHERTEXT_LEN};
use crate::encoder::{self, EncodeError};
use crate::wire::core_v1::{Message, Message_, Meta};
use crate::wire::crypto_v1::{Encrypted, SignedEnvelope};
use crate::wire::device_v1::{EncryptedUrban, Urban};
use crate::wire::limits::{
    MAX_MESSAGE_LEN, MAX_PRIVATE_PLAINTEXT_LEN, NONCE_LEN, NONCE_XCHACHA_LEN,
};
use crate::wire::{encode, signing_preimage};

/// UTC is not known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockError;

impl fmt::Display for ClockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UTC time is not available")
    }
}

impl core::error::Error for ClockError {}

/// Source of Unix UTC time.
///
/// Monotonic time is never UTC and must not be returned here: a clock that is
/// not synchronized reports [`ClockError`].
pub trait UtcClock {
    /// Milliseconds since the Unix epoch, UTC.
    ///
    /// # Errors
    ///
    /// [`ClockError`] while UTC is not known.
    fn now_unix_ms(&self) -> Result<u64, ClockError>;
}

impl<C: Clock, M: embassy_sync::blocking_mutex::raw::RawMutex> UtcClock
    for rohi_time::TimeService<C, M>
{
    fn now_unix_ms(&self) -> Result<u64, ClockError> {
        self.now().map(|time| time.as_millis()).ok_or(ClockError)
    }
}

/// No entropy could be obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntropyError;

/// Source of unpredictable bytes: the nonces of the envelope and of the
/// private sections. A nonce must never repeat under the same key.
pub trait Entropy {
    /// Fills `dst` with unpredictable bytes.
    ///
    /// # Errors
    ///
    /// [`EntropyError`] when no entropy is available.
    fn fill(&mut self, dst: &mut [u8]) -> Result<(), EntropyError>;
}

/// The device identity as the format uses it: public key, signature and key
/// agreement. Blanket-implemented for every
/// [`Signer`]` + `[`KeyAgreement`]; a store-backed identity that may be
/// absent implements it by hand and returns [`IdentityError::NotProvisioned`].
pub trait DeviceKey {
    /// The public key (`SignedEnvelope.sensor_id`).
    ///
    /// # Errors
    ///
    /// [`IdentityError::NotProvisioned`] without an identity.
    fn public_key(&self) -> Result<PublicKey, IdentityError>;

    /// Ed25519 signature over `message`.
    ///
    /// # Errors
    ///
    /// [`IdentityError`].
    fn sign(&self, message: &[u8]) -> Result<Signature, IdentityError>;

    /// Key agreement with a recipient.
    ///
    /// # Errors
    ///
    /// [`IdentityError`].
    fn agree(&self, peer: &PublicKey) -> Result<rohi_identity::SharedSecret, IdentityError>;
}

impl<T: Signer<Error = IdentityError> + KeyAgreement> DeviceKey for T {
    fn public_key(&self) -> Result<PublicKey, IdentityError> {
        Ok(Signer::public_key(self))
    }

    fn sign(&self, message: &[u8]) -> Result<Signature, IdentityError> {
        Signer::sign(self, message)
    }

    fn agree(&self, peer: &PublicKey) -> Result<rohi_identity::SharedSecret, IdentityError> {
        KeyAgreement::agree(self, peer)
    }
}

struct Agreement<'a, K>(&'a K);

impl<K: DeviceKey> KeyAgreement for Agreement<'_, K> {
    fn agree(&self, peer: &PublicKey) -> Result<rohi_identity::SharedSecret, IdentityError> {
        self.0.agree(peer)
    }
}

/// The Robonomics Connectivity Protocol as a [`PacketFormat`].
///
/// One call to [`seal`](PacketFormat::seal) serializes the public section,
/// encrypts every private section for its recipient, signs the whole
/// `core.v1.Message` with the device identity and writes the
/// `crypto.v1.SignedEnvelope` to the caller's buffer. Any failure fails the
/// whole packet: there is no unencrypted fallback and no partial packet.
///
/// The message timestamp is the UTC time of the capture boundary, computed
/// from the UTC clock and the monotonic clock; without UTC the packet is
/// refused. `Meta.node_id` stays zero: the device is identified by the
/// envelope's `sensor_id`.
pub struct RobonomicsFormat<'a, K, E, U, C> {
    key: &'a K,
    entropy: E,
    utc: &'a U,
    monotonic: C,
}

impl<'a, K: DeviceKey, E: Entropy, U: UtcClock, C: Clock> RobonomicsFormat<'a, K, E, U, C> {
    /// A format signing with `key`, drawing nonces from `entropy` and dating
    /// packets with `utc` and `monotonic`.
    pub const fn new(key: &'a K, entropy: E, utc: &'a U, monotonic: C) -> Self {
        Self {
            key,
            entropy,
            utc,
            monotonic,
        }
    }

    fn timestamp(&self, captured_at: MonotonicTimestamp) -> Result<u64, SealError> {
        let now = self
            .utc
            .now_unix_ms()
            .map_err(|_| SealError::TimeUnavailable)?;
        // The capture happened this long ago.
        let age = self.monotonic.now().saturating_millis_since(captured_at);
        now.checked_sub(age).ok_or(SealError::TimeUnavailable)
    }
}

fn map_identity(error: IdentityError) -> SealError {
    match error {
        IdentityError::NotProvisioned => SealError::IdentityUnavailable,
        _ => SealError::Signing,
    }
}

impl<K: DeviceKey, E: Entropy, U: UtcClock, C: Clock> PacketFormat
    for RobonomicsFormat<'_, K, E, U, C>
{
    const ID: PacketFormatId = PacketFormatId::Robonomics;

    fn supported(&self) -> Selection {
        encoder::supported()
    }

    fn accepts_recipient(&self, recipient: &RecipientAddress) -> bool {
        recipient_key(recipient).is_some()
    }

    async fn seal<'o, const N: usize, const R: usize>(
        &mut self,
        sections: &PreparedSections<'_, N, R>,
        out: &'o mut [u8],
    ) -> Result<SignedPacket<'o>, SealError> {
        let timestamp = self.timestamp(sections.captured_at)?;
        let sender = self.key.public_key().map_err(map_identity)?;

        let mut urban = Urban {
            public: encoder::encode_section(&sections.public).map_err(map_encode)?,
            ..Urban::default()
        };
        for section in &sections.private {
            let sensors = encoder::encode_section(&section.view).map_err(map_encode)?;
            if sensors.is_empty() {
                continue;
            }
            let encrypted = self.encrypt_section(&sender, &section.recipient, sensors)?;
            urban
                .private
                .push(encrypted)
                .map_err(|_| SealError::TooLarge)?;
        }
        if urban.public.is_empty() && urban.private.is_empty() {
            return Err(SealError::NothingToPackage);
        }

        let mut message = Message::default();
        message.set_metadata(Meta {
            node_id: 0,
            timestamp,
        });
        message.payload = Some(Message_::Payload::Urban(urban));
        let mut message_bytes = [0u8; MAX_MESSAGE_LEN];
        let message_len = encode(&message, &mut message_bytes).map_err(|_| SealError::TooLarge)?;

        let mut nonce = [0u8; NONCE_LEN];
        self.entropy
            .fill(&mut nonce)
            .map_err(|_| SealError::EntropyUnavailable)?;
        let mut preimage = [0u8; PUBLIC_KEY_LEN + NONCE_LEN + MAX_MESSAGE_LEN];
        let preimage_len = signing_preimage(
            sender.as_bytes(),
            &nonce,
            &message_bytes[..message_len],
            &mut preimage,
        )
        .map_err(|_| SealError::TooLarge)?;
        let signature = self
            .key
            .sign(&preimage[..preimage_len])
            .map_err(map_identity)?;

        let mut envelope = SignedEnvelope::default();
        let too_large = |_| SealError::TooLarge;
        envelope
            .sensor_id
            .extend_from_slice(sender.as_bytes())
            .map_err(too_large)?;
        envelope
            .nonce
            .extend_from_slice(&nonce)
            .map_err(too_large)?;
        envelope
            .message
            .extend_from_slice(&message_bytes[..message_len])
            .map_err(too_large)?;
        envelope
            .signature
            .extend_from_slice(signature.as_bytes())
            .map_err(too_large)?;
        let len = encode(&envelope, out).map_err(|_| SealError::BufferTooSmall)?;

        let info = PacketInfo {
            profile: sections.profile.clone(),
            format: Self::ID,
            length: len,
            captured_at: sections.captured_at,
        };
        Ok(SignedPacket::new(info, &out[..len]))
    }
}

fn map_encode(error: EncodeError) -> SealError {
    match error {
        EncodeError::Unsupported(id) => SealError::Unsupported(id),
        EncodeError::TooLarge => SealError::TooLarge,
    }
}

/// The key of `recipient` if it can be used for key agreement: a valid curve
/// point of full order.
fn recipient_key(recipient: &RecipientAddress) -> Option<PublicKey> {
    let point = curve25519_dalek::edwards::CompressedEdwardsY(recipient.0).decompress()?;
    (!point.is_small_order() && point.is_torsion_free()).then(|| PublicKey::from_bytes(recipient.0))
}

impl<K: DeviceKey, E: Entropy, U: UtcClock, C: Clock> RobonomicsFormat<'_, K, E, U, C> {
    /// One private section: `EncryptedUrban` -> `crypto.v1.Encrypted`, encoded.
    fn encrypt_section(
        &mut self,
        sender: &PublicKey,
        recipient: &RecipientAddress,
        sensors: encoder::Sensors,
    ) -> Result<heapless::Vec<u8, { crate::wire::limits::MAX_ENCRYPTED_LEN }>, SealError> {
        let recipient = recipient_key(recipient).ok_or(SealError::Encryption)?;
        let mut plaintext = [0u8; MAX_PRIVATE_PLAINTEXT_LEN];
        let plaintext_len =
            encode(&EncryptedUrban { sensors }, &mut plaintext).map_err(|_| SealError::TooLarge)?;

        let mut nonce = [0u8; NONCE_XCHACHA_LEN];
        self.entropy
            .fill(&mut nonce)
            .map_err(|_| SealError::EntropyUnavailable)?;
        let mut ciphertext = [0u8; MAX_CIPHERTEXT_LEN];
        let ciphertext_len = crypto::encrypt(
            &Agreement(self.key),
            &recipient,
            &nonce,
            &plaintext[..plaintext_len],
            &mut ciphertext,
        )
        .map_err(|error| match error {
            crypto::CryptoError::TooLarge => SealError::TooLarge,
            _ => SealError::Encryption,
        });
        zeroize::Zeroize::zeroize(&mut plaintext);
        let ciphertext_len = ciphertext_len?;

        let mut encrypted = Encrypted {
            version: crypto::VERSION,
            ..Encrypted::default()
        };
        encrypted
            .algorithm
            .push_str(crypto::ALGORITHM)
            .map_err(|_| SealError::Encryption)?;
        encrypted
            .from
            .extend_from_slice(sender.as_bytes())
            .map_err(|_| SealError::Encryption)?;
        encrypted
            .nonce
            .extend_from_slice(&nonce)
            .map_err(|_| SealError::Encryption)?;
        encrypted
            .ciphertext
            .extend_from_slice(&ciphertext[..ciphertext_len])
            .map_err(|_| SealError::TooLarge)?;
        let mut bytes = heapless::Vec::new();
        bytes
            .resize_default(crate::wire::limits::MAX_ENCRYPTED_LEN)
            .map_err(|_| SealError::TooLarge)?;
        let len = encode(&encrypted, &mut bytes).map_err(|_| SealError::TooLarge)?;
        bytes.truncate(len);
        Ok(bytes)
    }
}
