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
//! The administrator key of signed requests, on the host.
//!
//! A restricted TCP endpoint of a device trusts one Ed25519 public key. The
//! matching secret stays in a file of the administrator (a 32-byte seed as
//! 64 hex characters): it is never a command line argument and never logged.

use std::path::Path;

use rohi_identity::{Identity, IdentitySigner, PublicKey};
use zeroize::Zeroize;

use crate::error::{Error, Result};

/// An administrator key.
pub struct AdminKey {
    identity: Identity,
}

impl AdminKey {
    /// The key of a seed.
    pub fn from_seed(seed: &[u8; 32]) -> Result<Self> {
        let identity = Identity::from_secret(seed)
            .map_err(|_| Error::InvalidInput("the administrator key seed is invalid".into()))?;
        Ok(Self { identity })
    }

    /// A new random key and its seed, from the random source of the operating
    /// system.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] without such a source.
    pub fn generate() -> Result<(Self, [u8; 32])> {
        let mut seed = [0u8; 32];
        fill_random(&mut seed)?;
        let key = Self::from_seed(&seed)?;
        Ok((key, seed))
    }

    /// Reads the key file `path`: 64 hex characters, with or without `0x`.
    ///
    /// On Unix a file that other users can read is refused.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] for an unreadable, too open or malformed file.
    pub fn load(path: &Path) -> Result<Self> {
        check_private(path)?;
        let mut text = std::fs::read_to_string(path).map_err(|e| {
            Error::InvalidInput(format!(
                "cannot read the administrator key {}: {e}",
                path.display()
            ))
        })?;
        let parsed = parse_seed(&text);
        text.zeroize();
        let mut seed = parsed?;
        let key = Self::from_seed(&seed);
        seed.zeroize();
        key
    }

    /// The signer of requests.
    pub fn signer(&self) -> IdentitySigner {
        self.identity.signer()
    }

    /// The public key a device is told to trust.
    pub fn public_key(&self) -> PublicKey {
        self.identity.public_key()
    }
}

/// The text of a key file for `seed`.
pub fn seed_text(seed: &[u8; 32]) -> String {
    use std::fmt::Write as _;

    let mut text = String::with_capacity(65);
    for byte in seed {
        let _ = write!(text, "{byte:02x}");
    }
    text.push('\n');
    text
}

/// Writes a new key file that only its owner can read; an existing file is
/// never replaced.
///
/// # Errors
///
/// [`Error::InvalidInput`] when the file exists or cannot be written.
pub fn write_new(path: &Path, seed: &[u8; 32]) -> Result<()> {
    use std::io::Write as _;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut text = seed_text(seed);
    let written = options
        .open(path)
        .and_then(|mut file| file.write_all(text.as_bytes()));
    text.zeroize();
    written.map_err(|e| {
        Error::InvalidInput(format!(
            "cannot create the key file {}: {e}",
            path.display()
        ))
    })
}

fn parse_seed(text: &str) -> Result<[u8; 32]> {
    let invalid =
        || Error::InvalidInput("the administrator key file must hold 64 hex characters".into());
    let hex = text.trim();
    let hex = hex.strip_prefix("0x").unwrap_or(hex);
    if hex.len() != 64 || !hex.is_ascii() {
        return Err(invalid());
    }
    let mut seed = [0u8; 32];
    for (byte, pair) in seed.iter_mut().zip(hex.as_bytes().chunks(2)) {
        let pair = core::str::from_utf8(pair).map_err(|_| invalid())?;
        *byte = u8::from_str_radix(pair, 16).map_err(|_| invalid())?;
    }
    Ok(seed)
}

#[cfg(unix)]
fn check_private(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let mode = std::fs::metadata(path)
        .map_err(|e| {
            Error::InvalidInput(format!(
                "cannot read the administrator key {}: {e}",
                path.display()
            ))
        })?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        return Err(Error::InvalidInput(format!(
            "the administrator key {} can be read by other users; run `chmod 600 {}`",
            path.display(),
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_private(_: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn fill_random(out: &mut [u8]) -> Result<()> {
    use std::io::Read as _;

    std::fs::File::open("/dev/urandom")
        .and_then(|mut random| random.read_exact(out))
        .map_err(|e| Error::InvalidInput(format!("no random source: {e}")))
}

#[cfg(not(unix))]
fn fill_random(_: &mut [u8]) -> Result<()> {
    Err(Error::InvalidInput(
        "no random source: create the key file by hand".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seed_round_trips_through_its_text() {
        let seed = [0xab; 32];
        let text = seed_text(&seed);
        assert_eq!(parse_seed(&text).unwrap(), seed);
        assert_eq!(parse_seed(&format!("0x{}", text.trim())).unwrap(), seed);
    }

    #[test]
    fn a_malformed_seed_is_refused() {
        for text in ["", "abcd", &"zz".repeat(32), &"ab".repeat(33)] {
            assert!(parse_seed(text).is_err(), "{text}");
        }
    }

    #[test]
    fn the_key_file_is_private_and_never_replaced() {
        let dir = std::env::temp_dir().join(format!("rohi-admin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("admin.key");
        let _ = std::fs::remove_file(&path);
        let (key, seed) = AdminKey::generate().unwrap();
        write_new(&path, &seed).unwrap();
        assert!(write_new(&path, &seed).is_err());
        assert_eq!(
            AdminKey::load(&path).unwrap().public_key(),
            key.public_key()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(AdminKey::load(&path).is_err());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
