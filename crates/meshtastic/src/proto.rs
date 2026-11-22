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
//! The small part of the Meshtastic client protobufs this crate speaks.
//!
//! Field numbers are those of `meshtastic/mesh.proto`
//! (`ToRadio`, `FromRadio`, `MeshPacket`, `Data`, `Routing`, `MyNodeInfo`,
//! `NodeInfo`). Only what the transport needs is encoded or looked at;
//! every other field is skipped by its wire type, so a newer radio firmware
//! cannot break the decoder. Everything is bounded and allocation-free.

use crate::radio::{Outgoing, RoutingResult};

/// `PortNum::ROUTING_APP`: the port of routing acknowledgements.
const PORT_ROUTING_APP: u32 = 5;
/// `PortNum::TEXT_MESSAGE_APP`: the port of text messages, used by the diagnostics.
pub const PORT_TEXT_MESSAGE_APP: u32 = 1;

/// The output buffer is too small.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Overflow;

/// A message that is not valid protobuf of the expected shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Malformed;

struct Writer<'a> {
    out: &'a mut [u8],
    len: usize,
}

impl Writer<'_> {
    fn byte(&mut self, byte: u8) -> Result<(), Overflow> {
        *self.out.get_mut(self.len).ok_or(Overflow)? = byte;
        self.len += 1;
        Ok(())
    }

    fn bytes(&mut self, bytes: &[u8]) -> Result<(), Overflow> {
        let end = self.len.checked_add(bytes.len()).ok_or(Overflow)?;
        self.out
            .get_mut(self.len..end)
            .ok_or(Overflow)?
            .copy_from_slice(bytes);
        self.len = end;
        Ok(())
    }

    fn varint(&mut self, mut value: u64) -> Result<(), Overflow> {
        while value >= 0x80 {
            self.byte((value as u8 & 0x7f) | 0x80)?;
            value >>= 7;
        }
        self.byte(value as u8)
    }

    fn fixed32(&mut self, tag: u8, value: u32) -> Result<(), Overflow> {
        self.byte(tag)?;
        self.bytes(&value.to_le_bytes())
    }
}

fn varint_len(mut value: u64) -> usize {
    let mut len = 1;
    while value >= 0x80 {
        value >>= 7;
        len += 1;
    }
    len
}

/// Encodes `ToRadio { packet: MeshPacket { to, decoded: Data { portnum, payload }, id, want_ack,
/// pki_encrypted } }` into `out` and returns its length.
///
/// The channel stays 0 (required for PKI) and the destination is the unicast `to`.
///
/// # Errors
///
/// [`Overflow`] when `out` is too small.
pub fn encode_packet(out: &mut [u8], packet: &Outgoing<'_>) -> Result<usize, Overflow> {
    let data_len = 1
        + varint_len(u64::from(packet.port))
        + 1
        + varint_len(packet.payload.len() as u64)
        + packet.payload.len();
    let mesh_len = 5
        + 1
        + varint_len(data_len as u64)
        + data_len
        + 5
        + 2 * usize::from(packet.want_ack)
        + 3 * usize::from(packet.pki);
    let mut w = Writer { out, len: 0 };
    // ToRadio.packet (1)
    w.byte(0x0a)?;
    w.varint(mesh_len as u64)?;
    // MeshPacket.to (2)
    w.fixed32(0x15, packet.destination)?;
    // MeshPacket.decoded (4): Data { portnum (1), payload (2) }
    w.byte(0x22)?;
    w.varint(data_len as u64)?;
    w.byte(0x08)?;
    w.varint(u64::from(packet.port))?;
    w.byte(0x12)?;
    w.varint(packet.payload.len() as u64)?;
    w.bytes(packet.payload)?;
    // MeshPacket.id (6)
    w.fixed32(0x35, packet.id)?;
    // MeshPacket.want_ack (10)
    if packet.want_ack {
        w.bytes(&[0x50, 0x01])?;
    }
    // MeshPacket.pki_encrypted (17)
    if packet.pki {
        w.bytes(&[0x88, 0x01, 0x01])?;
    }
    Ok(w.len)
}

/// Encodes `ToRadio { want_config_id }`, which starts the configuration dump.
///
/// # Errors
///
/// [`Overflow`] when `out` is too small.
pub fn encode_want_config(out: &mut [u8], id: u32) -> Result<usize, Overflow> {
    let mut w = Writer { out, len: 0 };
    w.byte(0x18)?;
    w.varint(u64::from(id))?;
    Ok(w.len)
}

/// Encodes `ToRadio { heartbeat }`, which keeps the serial session alive.
///
/// # Errors
///
/// [`Overflow`] when `out` is too small.
pub fn encode_heartbeat(out: &mut [u8]) -> Result<usize, Overflow> {
    let mut w = Writer { out, len: 0 };
    w.bytes(&[0x3a, 0x00])?;
    Ok(w.len)
}

/// What a `FromRadio` message means to the transport and the diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FromRadio<'a> {
    /// The routing outcome of a packet, from the node `from`.
    Routing {
        /// The node that acknowledged (or refused) the packet.
        from: u32,
        /// The outcome.
        result: RoutingResult,
    },
    /// A text message (`TEXT_MESSAGE_APP`); the bytes are not checked to be UTF-8.
    Text {
        /// The sender.
        from: u32,
        /// The mesh packet ID.
        id: u32,
        /// The message.
        text: &'a [u8],
    },
    /// What the radio says about itself (`DeviceMetadata`).
    Metadata {
        /// The firmware version text.
        firmware: &'a [u8],
        /// The `HardwareModel` number.
        hardware: u32,
    },
    /// The number of the local node.
    MyInfo(u32),
    /// An entry of the node database.
    NodeInfo(u32),
    /// The configuration dump identified by the id is complete.
    ConfigComplete(u32),
    /// The radio restarted: its session is gone.
    Rebooted,
    /// Anything else.
    Other,
}

struct Reader<'a> {
    bytes: &'a [u8],
}

enum Field<'a> {
    Varint(u64),
    Fixed32(u32),
    Bytes(&'a [u8]),
    Skipped,
}

impl<'a> Reader<'a> {
    fn varint(&mut self) -> Result<u64, Malformed> {
        let mut value = 0u64;
        for shift in (0..70).step_by(7) {
            let (&byte, rest) = self.bytes.split_first().ok_or(Malformed)?;
            self.bytes = rest;
            value |= u64::from(byte & 0x7f).checked_shl(shift).ok_or(Malformed)?;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(Malformed)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], Malformed> {
        if len > self.bytes.len() {
            return Err(Malformed);
        }
        let (head, rest) = self.bytes.split_at(len);
        self.bytes = rest;
        Ok(head)
    }

    // The next field as (number, value); `None` at the end.
    fn next(&mut self) -> Result<Option<(u32, Field<'a>)>, Malformed> {
        if self.bytes.is_empty() {
            return Ok(None);
        }
        let key = self.varint()?;
        let number = u32::try_from(key >> 3).map_err(|_| Malformed)?;
        let field = match key & 7 {
            0 => Field::Varint(self.varint()?),
            1 => {
                self.take(8)?;
                Field::Skipped
            }
            2 => {
                let len = usize::try_from(self.varint()?).map_err(|_| Malformed)?;
                Field::Bytes(self.take(len)?)
            }
            5 => {
                let raw: [u8; 4] = self.take(4)?.try_into().map_err(|_| Malformed)?;
                Field::Fixed32(u32::from_le_bytes(raw))
            }
            _ => return Err(Malformed),
        };
        Ok(Some((number, field)))
    }
}

fn varint_u32(value: u64) -> Result<u32, Malformed> {
    u32::try_from(value).map_err(|_| Malformed)
}

/// Decodes the parts of a `FromRadio` message the transport uses.
///
/// # Errors
///
/// [`Malformed`] for truncated or invalid protobuf.
pub fn decode_from_radio(bytes: &[u8]) -> Result<FromRadio<'_>, Malformed> {
    let mut reader = Reader { bytes };
    let mut result = FromRadio::Other;
    while let Some((number, field)) = reader.next()? {
        match (number, field) {
            (2, Field::Bytes(packet)) => result = decode_packet(packet)?,
            (3, Field::Bytes(info)) => result = FromRadio::MyInfo(first_varint(info)?),
            (4, Field::Bytes(info)) => result = FromRadio::NodeInfo(first_varint(info)?),
            (7, Field::Varint(id)) => result = FromRadio::ConfigComplete(varint_u32(id)?),
            (13, Field::Bytes(metadata)) => result = decode_metadata(metadata)?,
            (8, Field::Varint(rebooted)) if rebooted != 0 => result = FromRadio::Rebooted,
            _ => {}
        }
    }
    Ok(result)
}

// Field 1 of `MyNodeInfo.my_node_num` and `NodeInfo.num`, both a uint32.
fn first_varint(bytes: &[u8]) -> Result<u32, Malformed> {
    let mut reader = Reader { bytes };
    while let Some((number, field)) = reader.next()? {
        if let (1, Field::Varint(value)) = (number, field) {
            return varint_u32(value);
        }
    }
    Ok(0)
}

fn decode_metadata(bytes: &[u8]) -> Result<FromRadio<'_>, Malformed> {
    let mut reader = Reader { bytes };
    let (mut firmware, mut hardware) = (&[][..], 0u32);
    while let Some((number, field)) = reader.next()? {
        match (number, field) {
            (1, Field::Bytes(version)) => firmware = version,
            (9, Field::Varint(model)) => hardware = varint_u32(model)?,
            _ => {}
        }
    }
    Ok(FromRadio::Metadata { firmware, hardware })
}

// A `MeshPacket` matters when it carries a routing result or a text message.
fn decode_packet(bytes: &[u8]) -> Result<FromRadio<'_>, Malformed> {
    let mut reader = Reader { bytes };
    let (mut from, mut id, mut data) = (0u32, 0u32, None);
    while let Some((number, field)) = reader.next()? {
        match (number, field) {
            (1, Field::Fixed32(value)) => from = value,
            (4, Field::Bytes(value)) => data = Some(value),
            (6, Field::Fixed32(value)) => id = value,
            _ => {}
        }
    }
    match data {
        Some(data) => decode_data(from, id, data),
        None => Ok(FromRadio::Other),
    }
}

fn decode_data(from: u32, id: u32, bytes: &[u8]) -> Result<FromRadio<'_>, Malformed> {
    let mut reader = Reader { bytes };
    let (mut port, mut payload, mut request_id) = (0u32, &[][..], 0u32);
    while let Some((number, field)) = reader.next()? {
        match (number, field) {
            (1, Field::Varint(value)) => port = varint_u32(value)?,
            (2, Field::Bytes(value)) => payload = value,
            (6, Field::Fixed32(value)) => request_id = value,
            _ => {}
        }
    }
    match port {
        PORT_TEXT_MESSAGE_APP => Ok(FromRadio::Text {
            from,
            id,
            text: payload,
        }),
        PORT_ROUTING_APP => decode_routing(from, request_id, payload),
        _ => Ok(FromRadio::Other),
    }
}

// `Routing`: only `error_reason` (3) matters; route discovery replies are not results.
fn decode_routing(from: u32, request_id: u32, payload: &[u8]) -> Result<FromRadio<'_>, Malformed> {
    let mut routing = Reader { bytes: payload };
    let mut error = 0u8;
    while let Some((number, field)) = routing.next()? {
        match (number, field) {
            (3, Field::Varint(value)) => error = u8::try_from(value).unwrap_or(u8::MAX),
            (1 | 2, Field::Bytes(_)) => return Ok(FromRadio::Other),
            _ => {}
        }
    }
    if request_id == 0 {
        return Ok(FromRadio::Other);
    }
    Ok(FromRadio::Routing {
        from,
        result: RoutingResult { request_id, error },
    })
}

#[cfg(test)]
mod tests;
