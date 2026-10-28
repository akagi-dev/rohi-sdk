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
///////////////////////////////////////////////////////////////////////////////

use core::fmt;

use minicbor::decode::{self, Decoder};
use minicbor::encode::{self, Encoder, Write};
use minicbor::{CborLen, Decode, Encode};
use zeroize::Zeroize;

/// Secret material that does not behave like ordinary data.
///
/// - [`Debug`] prints `Secret(<redacted>)`, there is no `Display`;
/// - the value is zeroized on drop;
/// - the value is only reachable through [`expose_secret`](Self::expose_secret).
///
/// Its CBOR encoding (a byte string for `Secret<[u8; N]>`) exists only to
/// persist it in the configuration store.
pub struct Secret<T: Zeroize>(T);

impl<T: Zeroize> Secret<T> {
    /// Wraps `value`.
    pub const fn new(value: T) -> Self {
        Self(value)
    }

    /// The raw secret. Never log, print or return it through a public API.
    pub fn expose_secret(&self) -> &T {
        &self.0
    }
}

impl<T: Zeroize> Drop for Secret<T> {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl<T: Zeroize + Clone> Clone for Secret<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: Zeroize> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl<C, const N: usize> Encode<C> for Secret<[u8; N]> {
    fn encode<W: Write>(
        &self,
        e: &mut Encoder<W>,
        _: &mut C,
    ) -> Result<(), encode::Error<W::Error>> {
        e.bytes(&self.0)?;
        Ok(())
    }
}

impl<'b, C, const N: usize> Decode<'b, C> for Secret<[u8; N]> {
    fn decode(d: &mut Decoder<'b>, _: &mut C) -> Result<Self, decode::Error> {
        let position = d.position();
        let bytes = d.bytes()?;
        let mut value = [0u8; N];
        if bytes.len() != N {
            return Err(decode::Error::message("unexpected secret length").at(position));
        }
        value.copy_from_slice(bytes);
        Ok(Self(value))
    }
}

impl<C, const N: usize> CborLen<C> for Secret<[u8; N]> {
    fn cbor_len(&self, ctx: &mut C) -> usize {
        // A byte string header for the length, then the bytes.
        N.cbor_len(ctx) + N
    }
}
