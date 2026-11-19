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
//! Recipient encryption of private sections, compatible with the Robonomics
//! CPS scheme of `airalab/robins` (`libcps`) for Ed25519 keys.
//!
//! ```text
//! shared = X25519(clamp(SHA-512(seed)[..32]), montgomery(recipient Ed25519 key))
//! key    = HKDF-SHA256(salt = "robonomics-network", ikm = shared,
//!                      info = "xchacha20poly1305") -> 32 bytes
//! sealed = XChaCha20-Poly1305(key, 24-byte random nonce, aad = empty, plaintext)
//! Encrypted { version: 1, algorithm: "xchacha20", from: sender key, nonce, ciphertext }
//! ```
//!
//! The Ed25519 key of the device is never converted to a sealed-box key
//! implicitly: [`KeyAgreement`] performs exactly the agreement above.

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use rohi_identity::{IdentityError, KeyAgreement, PublicKey};
use sha2::Sha256;
use zeroize::Zeroize;

use crate::wire::limits::{MAX_PRIVATE_PLAINTEXT_LEN, NONCE_XCHACHA_LEN};

/// The salt of the key derivation, shared with `libcps`.
pub const HKDF_SALT: &[u8] = b"robonomics-network";
/// The HKDF `info` of XChaCha20-Poly1305, shared with `libcps`.
pub const HKDF_INFO: &[u8] = b"xchacha20poly1305";
/// The `Encrypted.algorithm` text of XChaCha20-Poly1305.
pub const ALGORITHM: &str = "xchacha20";
/// The `Encrypted.version`.
pub const VERSION: u32 = 1;
/// Size of the Poly1305 tag appended to the ciphertext.
pub const TAG_LEN: usize = 16;
/// Largest ciphertext, in bytes.
pub const MAX_CIPHERTEXT_LEN: usize = MAX_PRIVATE_PLAINTEXT_LEN + TAG_LEN;

/// Why a section could not be encrypted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CryptoError {
    /// The recipient key is not usable for key agreement.
    Recipient,
    /// The device key is not available.
    Identity,
    /// The plaintext does not fit [`MAX_PRIVATE_PLAINTEXT_LEN`].
    TooLarge,
    /// The cipher failed.
    Cipher,
}

impl From<IdentityError> for CryptoError {
    fn from(error: IdentityError) -> Self {
        match error {
            IdentityError::InvalidPublicKey => Self::Recipient,
            _ => Self::Identity,
        }
    }
}

fn key_for(
    agreement: &impl KeyAgreement,
    peer: &PublicKey,
) -> Result<chacha20poly1305::Key, CryptoError> {
    let shared = agreement.agree(peer)?;
    let mut key = chacha20poly1305::Key::default();
    let hkdf = Hkdf::<Sha256>::new(Some(HKDF_SALT), shared.expose());
    hkdf.expand(HKDF_INFO, &mut key)
        .map_err(|_| CryptoError::Cipher)?;
    Ok(key)
}

/// Encrypts `plaintext` for `recipient` with the identity behind `agreement`.
/// Returns the ciphertext with the tag appended, in `out`, and its length.
///
/// `nonce` must be 24 unpredictable bytes used for no other message under the
/// same key pair.
///
/// # Errors
///
/// [`CryptoError`]; nothing usable is left in `out` on failure.
pub fn encrypt(
    agreement: &impl KeyAgreement,
    recipient: &PublicKey,
    nonce: &[u8; NONCE_XCHACHA_LEN],
    plaintext: &[u8],
    out: &mut [u8; MAX_CIPHERTEXT_LEN],
) -> Result<usize, CryptoError> {
    if plaintext.len() > MAX_PRIVATE_PLAINTEXT_LEN {
        return Err(CryptoError::TooLarge);
    }
    let mut key = key_for(agreement, recipient)?;
    let cipher = XChaCha20Poly1305::new(&key);
    key.zeroize();
    out[..plaintext.len()].copy_from_slice(plaintext);
    let tag = match cipher.encrypt_in_place_detached(
        &XNonce::from(*nonce),
        &[],
        &mut out[..plaintext.len()],
    ) {
        Ok(tag) => tag,
        Err(_) => {
            out.zeroize();
            return Err(CryptoError::Cipher);
        }
    };
    out[plaintext.len()..plaintext.len() + TAG_LEN].copy_from_slice(&tag);
    Ok(plaintext.len() + TAG_LEN)
}

/// Decrypts a section the way a recipient does; used by the tests and by host
/// tools that hold the recipient identity. Returns the plaintext length.
///
/// # Errors
///
/// [`CryptoError::Cipher`] for a wrong recipient, a tampered section or a
/// wrong sender key.
pub fn decrypt(
    agreement: &impl KeyAgreement,
    sender: &PublicKey,
    nonce: &[u8; NONCE_XCHACHA_LEN],
    ciphertext: &[u8],
    out: &mut [u8; MAX_PRIVATE_PLAINTEXT_LEN],
) -> Result<usize, CryptoError> {
    let len = ciphertext
        .len()
        .checked_sub(TAG_LEN)
        .filter(|len| *len <= MAX_PRIVATE_PLAINTEXT_LEN)
        .ok_or(CryptoError::TooLarge)?;
    let mut key = key_for(agreement, sender)?;
    let cipher = XChaCha20Poly1305::new(&key);
    key.zeroize();
    out[..len].copy_from_slice(&ciphertext[..len]);
    let mut tag_bytes = [0u8; TAG_LEN];
    tag_bytes.copy_from_slice(&ciphertext[len..]);
    let tag = chacha20poly1305::Tag::from(tag_bytes);
    match cipher.decrypt_in_place_detached(&XNonce::from(*nonce), &[], &mut out[..len], &tag) {
        Ok(()) => Ok(len),
        Err(_) => {
            out.zeroize();
            Err(CryptoError::Cipher)
        }
    }
}
