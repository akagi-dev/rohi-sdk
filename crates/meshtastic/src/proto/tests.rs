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
use std::vec::Vec;

use super::*;

fn varint(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
    out
}

fn field(number: u32, wire: u8, body: &[u8]) -> Vec<u8> {
    let mut out = varint(u64::from(number) << 3 | u64::from(wire));
    if wire == 2 {
        out.extend(varint(body.len() as u64));
    }
    out.extend_from_slice(body);
    out
}

fn routing_packet(request_id: u32, error: Option<u64>, port: u64) -> Vec<u8> {
    let routing = error.map_or_else(Vec::new, |error| field(3, 0, &varint(error)));
    let mut data = field(1, 0, &varint(port));
    data.extend(field(2, 2, &routing));
    data.extend(field(6, 5, &request_id.to_le_bytes()));
    let mut packet = field(1, 5, &0x1111_1111u32.to_le_bytes());
    packet.extend(field(4, 2, &data));
    field(2, 2, &packet)
}

#[test]
fn a_packet_is_encoded_as_to_radio_mesh_packet() {
    let packet = Outgoing {
        destination: 0xdead_beef,
        id: 0x0102_0304,
        port: 256,
        payload: &[0x01, 0xaa],
        want_ack: true,
        pki: true,
    };
    let mut out = [0u8; 64];
    let len = encode_packet(&mut out, &packet).unwrap();
    assert_eq!(
        out[..len],
        [
            0x0a, 0x18, // ToRadio.packet, 24 bytes
            0x15, 0xef, 0xbe, 0xad, 0xde, // to
            0x22, 0x07, 0x08, 0x80, 0x02, 0x12, 0x02, 0x01,
            0xaa, // decoded: port 256, payload
            0x35, 0x04, 0x03, 0x02, 0x01, // id
            0x50, 0x01, // want_ack
            0x88, 0x01, 0x01, // pki_encrypted
        ]
    );
}

#[test]
fn the_largest_frame_fits_the_declared_buffer() {
    let payload = [0u8; 219];
    let packet = Outgoing {
        destination: u32::MAX - 1,
        id: u32::MAX,
        port: 256,
        payload: &payload,
        want_ack: true,
        pki: true,
    };
    let mut out = [0u8; 256];
    let len = encode_packet(&mut out, &packet).unwrap();
    assert!(len <= 256, "{len}");
    assert_eq!(encode_packet(&mut out[..len - 1], &packet), Err(Overflow));
}

#[test]
fn want_config_and_heartbeat_are_the_documented_to_radio_fields() {
    let mut out = [0u8; 8];
    let len = encode_want_config(&mut out, 300).unwrap();
    assert_eq!(out[..len], [0x18, 0xac, 0x02]);
    let len = encode_heartbeat(&mut out).unwrap();
    assert_eq!(out[..len], [0x3a, 0x00]);
}

#[test]
fn a_routing_ack_is_a_result_and_an_error_carries_its_code() {
    assert_eq!(
        decode_from_radio(&routing_packet(0x2a, Some(0), 5)),
        Ok(FromRadio::Routing {
            from: 0x1111_1111,
            result: RoutingResult {
                request_id: 0x2a,
                error: 0
            }
        })
    );
    // `error_reason` omitted means NONE.
    assert_eq!(
        decode_from_radio(&routing_packet(7, None, 5)),
        Ok(FromRadio::Routing {
            from: 0x1111_1111,
            result: RoutingResult {
                request_id: 7,
                error: 0
            }
        })
    );
    assert_eq!(
        decode_from_radio(&routing_packet(7, Some(35), 5)),
        Ok(FromRadio::Routing {
            from: 0x1111_1111,
            result: RoutingResult {
                request_id: 7,
                error: 35
            }
        })
    );
}

#[test]
fn other_ports_and_replies_are_not_results() {
    // Another application port, even with a request id.
    assert_eq!(
        decode_from_radio(&routing_packet(7, Some(0), 256)),
        Ok(FromRadio::Other)
    );
    // A routing packet without a request id answers nothing of ours.
    assert_eq!(
        decode_from_radio(&routing_packet(0, Some(0), 5)),
        Ok(FromRadio::Other)
    );
}

#[test]
fn the_session_messages_are_recognised() {
    assert_eq!(
        decode_from_radio(&field(3, 2, &field(1, 0, &varint(0xdead_beef)))),
        Ok(FromRadio::MyInfo(0xdead_beef))
    );
    assert_eq!(
        decode_from_radio(&field(4, 2, &field(1, 0, &varint(0x1234)))),
        Ok(FromRadio::NodeInfo(0x1234))
    );
    assert_eq!(
        decode_from_radio(&field(7, 0, &varint(99))),
        Ok(FromRadio::ConfigComplete(99))
    );
    assert_eq!(
        decode_from_radio(&field(8, 0, &[1])),
        Ok(FromRadio::Rebooted)
    );
    // The id (1) and unknown fields do not matter.
    let mut message = field(1, 0, &varint(5));
    message.extend(field(15, 2, b"file"));
    message.extend(field(7, 0, &varint(1)));
    assert_eq!(
        decode_from_radio(&message),
        Ok(FromRadio::ConfigComplete(1))
    );
}

#[test]
fn truncated_or_invalid_messages_are_malformed_not_panics() {
    let good = routing_packet(9, Some(0), 5);
    for len in 1..good.len() {
        // Every prefix either decodes to something or is malformed; none panics.
        let _ = decode_from_radio(&good[..len]);
    }
    assert_eq!(decode_from_radio(&good[..good.len() - 1]), Err(Malformed));
    // Group wire types are not part of proto3.
    assert_eq!(decode_from_radio(&[0x0b]), Err(Malformed));
    // A varint that never ends.
    assert_eq!(
        decode_from_radio(&[
            0x08, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff
        ]),
        Err(Malformed)
    );
    assert_eq!(decode_from_radio(&[]), Ok(FromRadio::Other));
}

#[test]
fn text_messages_and_radio_metadata_are_recognised() {
    let mut data = field(1, 0, &varint(1));
    data.extend(field(2, 2, b"Hello ROHI"));
    let mut packet = field(1, 5, &0xdead_beefu32.to_le_bytes());
    packet.extend(field(4, 2, &data));
    packet.extend(field(6, 5, &15230u32.to_le_bytes()));
    assert_eq!(
        decode_from_radio(&field(2, 2, &packet)),
        Ok(FromRadio::Text {
            from: 0xdead_beef,
            id: 15230,
            text: b"Hello ROHI"
        })
    );

    let mut metadata = field(1, 2, b"2.5.9.abcdef");
    metadata.extend(field(9, 0, &varint(43)));
    assert_eq!(
        decode_from_radio(&field(13, 2, &metadata)),
        Ok(FromRadio::Metadata {
            firmware: b"2.5.9.abcdef",
            hardware: 43
        })
    );
}
