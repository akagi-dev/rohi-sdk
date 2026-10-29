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
//! CBOR codecs of the bounded containers the persisted types are made of, for
//! `#[cbor(encode_with = "...", decode_with = "...", cbor_len = "...")]`.
//!
//! A value that does not fit the bound is a decode error, never cut:
//!
//! ```ignore
//! #[derive(minicbor::Encode, minicbor::Decode, minicbor::CborLen)]
//! #[cbor(transparent)]
//! pub struct Name(
//!     #[n(0)]
//!     #[cbor(
//!         encode_with = "rohi_config::cbor::text::encode",
//!         decode_with = "rohi_config::cbor::text::decode",
//!         cbor_len = "rohi_config::cbor::text::cbor_len"
//!     )]
//!     heapless::String<32>,
//! );
//! ```

/// A bounded text, a CBOR text string.
pub mod text {
    use heapless::String;
    use minicbor::CborLen;
    use minicbor::decode::{self, Decoder};
    use minicbor::encode::{self, Encoder, Write};

    /// Encodes the text.
    pub fn encode<C, W: Write, const N: usize>(
        value: &String<N>,
        e: &mut Encoder<W>,
        _: &mut C,
    ) -> Result<(), encode::Error<W::Error>> {
        e.str(value)?;
        Ok(())
    }

    /// Decodes a text of at most `N` bytes.
    pub fn decode<C, const N: usize>(
        d: &mut Decoder<'_>,
        _: &mut C,
    ) -> Result<String<N>, decode::Error> {
        let position = d.position();
        String::try_from(d.str()?).map_err(|_| decode::Error::message("text too long").at(position))
    }

    /// The encoded length of the text.
    pub fn cbor_len<C, const N: usize>(value: &String<N>, ctx: &mut C) -> usize {
        <&str as CborLen<C>>::cbor_len(&value.as_str(), ctx)
    }
}

/// A bounded byte vector, a CBOR byte string.
pub mod bytes {
    use heapless::Vec;
    use minicbor::CborLen;
    use minicbor::bytes::ByteSlice;
    use minicbor::decode::{self, Decoder};
    use minicbor::encode::{self, Encoder, Write};

    /// Encodes the bytes.
    pub fn encode<C, W: Write, const N: usize>(
        value: &Vec<u8, N>,
        e: &mut Encoder<W>,
        _: &mut C,
    ) -> Result<(), encode::Error<W::Error>> {
        e.bytes(value)?;
        Ok(())
    }

    /// Decodes at most `N` bytes.
    pub fn decode<C, const N: usize>(
        d: &mut Decoder<'_>,
        _: &mut C,
    ) -> Result<Vec<u8, N>, decode::Error> {
        let position = d.position();
        Vec::from_slice(d.bytes()?)
            .map_err(|_| decode::Error::message("byte string too long").at(position))
    }

    /// The encoded length of the bytes.
    pub fn cbor_len<C, const N: usize>(value: &Vec<u8, N>, ctx: &mut C) -> usize {
        <&ByteSlice>::from(value.as_slice()).cbor_len(ctx)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use heapless::{String, Vec};
    use minicbor::{CborLen, Decode, Encode};

    use crate::{decode_exact, encode_to_slice};

    #[derive(Clone, Debug, PartialEq, Encode, Decode, CborLen)]
    #[cbor(array)]
    struct Sample {
        #[n(0)]
        #[cbor(
            encode_with = "super::text::encode",
            decode_with = "super::text::decode",
            cbor_len = "super::text::cbor_len"
        )]
        name: String<4>,
        #[n(1)]
        #[cbor(
            encode_with = "super::bytes::encode",
            decode_with = "super::bytes::decode",
            cbor_len = "super::bytes::cbor_len"
        )]
        data: Vec<u8, 3>,
    }

    fn sample(name: &str, data: &[u8]) -> Sample {
        Sample {
            name: String::try_from(name).unwrap(),
            data: Vec::from_slice(data).unwrap(),
        }
    }

    #[test]
    fn a_text_and_bytes_are_cbor_strings() {
        let value = sample("ab", &[1, 2]);
        let mut buffer = [0u8; 16];
        let used = encode_to_slice(&value, &mut buffer).unwrap();
        assert_eq!(used, minicbor::len(&value));
        // [ "ab", h'0102' ]
        assert_eq!(&buffer[..used], [0x82, 0x62, b'a', b'b', 0x42, 1, 2]);
        assert_eq!(decode_exact::<Sample>(&buffer[..used]), Ok(value));
    }

    #[test]
    fn what_does_not_fit_is_an_error_and_never_cut() {
        // "abcde" does not fit String<4>.
        let long_text = [0x82, 0x65, b'a', b'b', b'c', b'd', b'e', 0x40];
        assert!(decode_exact::<Sample>(&long_text).is_err());
        // Four bytes do not fit Vec<u8, 3>.
        let long_bytes = [0x82, 0x60, 0x44, 1, 2, 3, 4];
        assert!(decode_exact::<Sample>(&long_bytes).is_err());
    }
}
