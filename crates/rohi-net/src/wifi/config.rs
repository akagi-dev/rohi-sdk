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

use core::fmt;

use heapless::Vec;
use minicbor::bytes::ByteSlice;
use minicbor::decode::{self, Decoder};
use minicbor::encode::{self, Encoder, Write};
use minicbor::{CborLen, Decode, Encode};

use crate::{MAX_SSID_LEN, MAX_WIFI_PASSWORD_LEN};

/// CBOR codec of a bounded byte vector: a CBOR byte string.
mod bytes_codec {
    use super::*;

    pub fn encode<C, W: Write, const N: usize>(
        value: &Vec<u8, N>,
        e: &mut Encoder<W>,
        _: &mut C,
    ) -> Result<(), encode::Error<W::Error>> {
        e.bytes(value)?;
        Ok(())
    }

    pub fn decode<C, const N: usize>(
        d: &mut Decoder<'_>,
        _: &mut C,
    ) -> Result<Vec<u8, N>, decode::Error> {
        let position = d.position();
        Vec::from_slice(d.bytes()?)
            .map_err(|_| decode::Error::message("byte string too long").at(position))
    }

    pub fn cbor_len<C, const N: usize>(value: &Vec<u8, N>, ctx: &mut C) -> usize {
        <&ByteSlice>::from(value.as_slice()).cbor_len(ctx)
    }
}

/// Wi-Fi network name: 0..=[`MAX_SSID_LEN`] arbitrary bytes.
#[derive(Clone, PartialEq, Eq, Encode, Decode, CborLen)]
#[cbor(transparent)]
pub struct WifiSsid(
    #[n(0)]
    #[cbor(
        encode_with = "bytes_codec::encode",
        decode_with = "bytes_codec::decode",
        cbor_len = "bytes_codec::cbor_len"
    )]
    Vec<u8, MAX_SSID_LEN>,
);

impl WifiSsid {
    pub const fn empty() -> Self {
        Self(Vec::new())
    }

    /// Returns `None` when `value` is longer than [`MAX_SSID_LEN`].
    pub fn from_bytes(value: &[u8]) -> Option<Self> {
        Vec::from_slice(value).ok().map(Self)
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }

    /// The SSID as text, if it is valid UTF-8.
    pub fn as_str(&self) -> Option<&str> {
        core::str::from_utf8(self.as_bytes()).ok()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for WifiSsid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.as_str() {
            Some(text) => write!(f, "{text:?}"),
            None => write!(f, "{:02x?}", self.as_bytes()),
        }
    }
}

impl fmt::Display for WifiSsid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.as_str() {
            Some(text) => f.write_str(text),
            None => write!(f, "{:02x?}", self.as_bytes()),
        }
    }
}

/// Wi-Fi password. Secret: its `Debug` output never reveals the value.
#[derive(Clone, PartialEq, Eq, Encode, Decode, CborLen)]
#[cbor(transparent)]
pub struct WifiPassword(
    #[n(0)]
    #[cbor(
        encode_with = "bytes_codec::encode",
        decode_with = "bytes_codec::decode",
        cbor_len = "bytes_codec::cbor_len"
    )]
    Vec<u8, MAX_WIFI_PASSWORD_LEN>,
);

impl WifiPassword {
    /// Returns `None` when `value` is longer than [`MAX_WIFI_PASSWORD_LEN`].
    pub fn from_bytes(value: &[u8]) -> Option<Self> {
        Vec::from_slice(value).ok().map(Self)
    }

    /// The raw secret. Only the platform backend should call this.
    pub fn expose_secret(&self) -> &[u8] {
        self.0.as_slice()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for WifiPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WifiPassword(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_values_are_rejected() {
        assert!(WifiSsid::from_bytes(&[0; MAX_SSID_LEN + 1]).is_none());
        assert!(WifiPassword::from_bytes(&[0; MAX_WIFI_PASSWORD_LEN + 1]).is_none());
        // An oversized byte string in a stored payload must not panic.
        let mut payload = [0u8; 40];
        payload[0] = 0x58;
        payload[1] = 33;
        assert!(rohi_config::decode_exact::<WifiSsid>(&payload[..35]).is_err());
        assert!(rohi_config::decode_exact::<WifiSsid>(&payload[..34]).is_err());
    }
}
