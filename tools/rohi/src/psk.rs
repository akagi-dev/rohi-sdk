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
//! The pre-shared key of the TCP endpoint, on the host.
//!
//! A restricted TCP endpoint of a device authenticates its clients with one 32-byte secret,
//! see [`rohi_control_tcp::secure`]. The secret lives in a file of the user (64 hex
//! characters, readable by its owner only): it is never a command line argument, never logged
//! and wiped from memory when dropped.

use std::path::Path;

use zeroize::Zeroize;

use crate::error::{Error, Result};

/// A pre-shared key.
pub struct TcpKey([u8; 32]);

impl Drop for TcpKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl TcpKey {
    /// The key of `bytes`; all zeros is not a key.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] for 32 zero bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Result<Self> {
        if bytes.iter().all(|byte| *byte == 0) {
            return Err(Error::InvalidInput("the key must not be all zeros".into()));
        }
        Ok(Self(bytes))
    }

    /// A new random key, from the random source of the operating system.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] without such a source.
    pub fn generate() -> Result<Self> {
        let mut bytes = [0u8; 32];
        fill_random(&mut bytes)?;
        let key = Self::from_bytes(bytes);
        bytes.zeroize();
        key
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
            Error::InvalidInput(format!("cannot read the key {}: {e}", path.display()))
        })?;
        let parsed = parse_key(&text);
        text.zeroize();
        let mut bytes = parsed?;
        let key = Self::from_bytes(bytes);
        bytes.zeroize();
        key
    }

    /// The key material, to derive a session from or to send to a device over a protected link.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Fills `out` with random bytes from the operating system.
///
/// # Errors
///
/// [`Error::InvalidInput`] without a random source.
pub fn random(out: &mut [u8]) -> Result<()> {
    fill_random(out)
}

/// The text of a key file for `key`.
pub fn key_text(key: &[u8; 32]) -> String {
    use std::fmt::Write as _;

    let mut text = String::with_capacity(65);
    for byte in key {
        let _ = write!(text, "{byte:02x}");
    }
    text.push('\n');
    text
}

/// Writes a new key file that only its owner can read; an existing file is never replaced.
///
/// # Errors
///
/// [`Error::InvalidInput`] when the file exists or cannot be written.
pub fn write_new(path: &Path, key: &[u8; 32]) -> Result<()> {
    use std::io::Write as _;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut text = key_text(key);
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

fn parse_key(text: &str) -> Result<[u8; 32]> {
    let invalid = || Error::InvalidInput("the key file must hold 64 hex characters".into());
    let hex = text.trim();
    let hex = hex.strip_prefix("0x").unwrap_or(hex);
    if hex.len() != 64 || !hex.is_ascii() {
        return Err(invalid());
    }
    let mut key = [0u8; 32];
    for (byte, pair) in key.iter_mut().zip(hex.as_bytes().chunks(2)) {
        let pair = core::str::from_utf8(pair).map_err(|_| invalid())?;
        *byte = u8::from_str_radix(pair, 16).map_err(|_| invalid())?;
    }
    Ok(key)
}

#[cfg(unix)]
fn check_private(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let mode = std::fs::metadata(path)
        .map_err(|e| Error::InvalidInput(format!("cannot read the key {}: {e}", path.display())))?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        return Err(Error::InvalidInput(format!(
            "the key {} can be read by other users; run `chmod 600 {}`",
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
    fn a_key_round_trips_through_its_text() {
        let key = [0xab; 32];
        let text = key_text(&key);
        assert_eq!(parse_key(&text).unwrap(), key);
        assert_eq!(parse_key(&format!("0x{}", text.trim())).unwrap(), key);
    }

    #[test]
    fn a_malformed_or_empty_key_is_refused() {
        for text in ["", "abcd", &"zz".repeat(32), &"ab".repeat(33)] {
            assert!(parse_key(text).is_err(), "{text}");
        }
        assert!(TcpKey::from_bytes([0; 32]).is_err());
    }

    #[test]
    fn the_key_file_is_private_and_never_replaced() {
        let dir = std::env::temp_dir().join(format!("rohi-psk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tcp.key");
        let _ = std::fs::remove_file(&path);
        let key = TcpKey::generate().unwrap();
        write_new(&path, key.as_bytes()).unwrap();
        assert!(write_new(&path, key.as_bytes()).is_err());
        assert_eq!(TcpKey::load(&path).unwrap().as_bytes(), key.as_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(TcpKey::load(&path).is_err());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
