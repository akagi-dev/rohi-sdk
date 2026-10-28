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
//! CBOR codec of a bounded text: a CBOR text string.

use heapless::String;
use minicbor::CborLen;
use minicbor::decode::{self, Decoder};
use minicbor::encode::{self, Encoder, Write};

pub fn encode<C, W: Write, const N: usize>(
    value: &String<N>,
    e: &mut Encoder<W>,
    _: &mut C,
) -> Result<(), encode::Error<W::Error>> {
    e.str(value)?;
    Ok(())
}

pub fn decode<C, const N: usize>(
    d: &mut Decoder<'_>,
    _: &mut C,
) -> Result<String<N>, decode::Error> {
    let position = d.position();
    String::try_from(d.str()?).map_err(|_| decode::Error::message("text too long").at(position))
}

pub fn cbor_len<C, const N: usize>(value: &String<N>, ctx: &mut C) -> usize {
    <&str as CborLen<C>>::cbor_len(&value.as_str(), ctx)
}
