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

use super::*;

const PSK: [u8; PSK_LEN] = [0x42; PSK_LEN];

fn pair() -> (Channel, Channel) {
    let (client, hello) = ClientHandshake::start(&PSK, &[1; NONCE_LEN]);
    let (server, server_hello) = ServerHandshake::respond(&PSK, &hello, &[2; NONCE_LEN]).unwrap();
    let (client, finish) = client.on_server_hello(&server_hello).unwrap();
    (client, server.finish(&finish).unwrap())
}

// A record as it travels: header, data, tag.
fn seal(channel: &mut Channel, data: &[u8]) -> std::vec::Vec<u8> {
    let mut buffer = data.to_vec();
    let (header, tag) = channel.seal(&mut buffer).unwrap();
    [header.as_slice(), &buffer, &tag].concat()
}

fn open(
    channel: &mut Channel,
    record: &[u8],
    max: usize,
) -> Result<std::vec::Vec<u8>, RecordError> {
    let header: [u8; RECORD_HEADER_LEN] = record[..RECORD_HEADER_LEN].try_into().unwrap();
    let len = channel.open_header(header, max)?;
    // A record that is shorter than its header says is a truncated one.
    if record.len() != RECORD_HEADER_LEN + len + TAG_LEN {
        return Err(RecordError::Length);
    }
    let mut data = record[RECORD_HEADER_LEN..RECORD_HEADER_LEN + len].to_vec();
    let tag: [u8; TAG_LEN] = record[RECORD_HEADER_LEN + len..].try_into().unwrap();
    channel.open(header, &mut data, &tag)?;
    Ok(data)
}

#[test]
fn the_protocol_is_pinned_by_a_known_answer() {
    let (_, hello) = ClientHandshake::start(&PSK, &[1; NONCE_LEN]);
    assert_eq!(&hello[..5], b"ROHI\x01");
    let (_, server_hello) = ServerHandshake::respond(&PSK, &hello, &[2; NONCE_LEN]).unwrap();
    let hex: std::string::String = server_hello[5 + NONCE_LEN..]
        .iter()
        .map(|byte| std::format!("{byte:02x}"))
        .collect();
    assert_eq!(
        hex,
        "a2b53a85f6da810e1e279794ea16956a027dcdb17104b61f90b1c839567ad08f"
    );
}

#[test]
fn records_flow_both_ways_and_stay_secret() {
    let (mut client, mut server) = pair();
    for round in 0..50u8 {
        let request = std::vec![round; usize::from(round) + 1];
        let record = seal(&mut client, &request);
        assert_ne!(
            &record[RECORD_HEADER_LEN..RECORD_HEADER_LEN + request.len()],
            request
        );
        assert_eq!(open(&mut server, &record, 2048).unwrap(), request);

        let response = std::vec![round ^ 0xff; 300];
        let record = seal(&mut server, &response);
        assert_eq!(open(&mut client, &record, 2048).unwrap(), response);
    }
}

#[test]
fn another_key_is_refused_by_both_sides() {
    let (client, hello) = ClientHandshake::start(&[1; PSK_LEN], &[1; NONCE_LEN]);
    let (server, server_hello) = ServerHandshake::respond(&PSK, &hello, &[2; NONCE_LEN]).unwrap();
    // The client does not accept a device that does not know its key.
    assert_eq!(
        client.on_server_hello(&server_hello).err(),
        Some(HandshakeError::Proof)
    );
    // A client that does not know the key of the device is refused at its final message, and nothing
    // was processed before.
    assert_eq!(
        server.finish(&[0u8; CLIENT_FINISH_LEN]).err(),
        Some(HandshakeError::Proof)
    );
}

#[test]
fn a_replayed_or_altered_final_message_does_not_authenticate() {
    // The proof of one connection is useless in another: the nonces differ.
    let (client, hello) = ClientHandshake::start(&PSK, &[1; NONCE_LEN]);
    let (server, server_hello) = ServerHandshake::respond(&PSK, &hello, &[2; NONCE_LEN]).unwrap();
    let (_, finish) = client.on_server_hello(&server_hello).unwrap();

    let (_, other_hello) = ClientHandshake::start(&PSK, &[3; NONCE_LEN]);
    let (other, _) = ServerHandshake::respond(&PSK, &other_hello, &[2; NONCE_LEN]).unwrap();
    assert_eq!(other.finish(&finish).err(), Some(HandshakeError::Proof));
    let (again, _) = ServerHandshake::respond(&PSK, &hello, &[4; NONCE_LEN]).unwrap();
    assert_eq!(again.finish(&finish).err(), Some(HandshakeError::Proof));

    // One flipped bit anywhere in the proof.
    for bit in 0..CLIENT_FINISH_LEN * 8 {
        let (server, _) = ServerHandshake::respond(&PSK, &hello, &[2; NONCE_LEN]).unwrap();
        let mut altered = finish;
        altered[bit / 8] ^= 1 << (bit % 8);
        assert!(server.finish(&altered).is_err(), "bit {bit}");
    }
    assert!(server.finish(&finish).is_ok());
}

#[test]
fn a_server_hello_that_was_tampered_with_is_refused() {
    let (_, hello) = ClientHandshake::start(&PSK, &[1; NONCE_LEN]);
    let (_, server_hello) = ServerHandshake::respond(&PSK, &hello, &[2; NONCE_LEN]).unwrap();
    for index in 5..SERVER_HELLO_LEN {
        let (client, _) = ClientHandshake::start(&PSK, &[1; NONCE_LEN]);
        let mut altered = server_hello;
        altered[index] ^= 0x10;
        assert_eq!(
            client.on_server_hello(&altered).err(),
            Some(HandshakeError::Proof),
            "byte {index}"
        );
    }
}

#[test]
fn another_protocol_or_version_is_refused_before_any_key_is_used() {
    let (_, mut hello) = ClientHandshake::start(&PSK, &[1; NONCE_LEN]);
    hello[0] = b'X';
    assert_eq!(
        ServerHandshake::respond(&PSK, &hello, &[2; NONCE_LEN]).err(),
        Some(HandshakeError::Magic)
    );
    hello[0] = b'R';
    hello[4] = 2;
    assert_eq!(
        ServerHandshake::respond(&PSK, &hello, &[2; NONCE_LEN]).err(),
        Some(HandshakeError::Version)
    );
    let (client, hello) = ClientHandshake::start(&PSK, &[1; NONCE_LEN]);
    let (_, mut server_hello) = ServerHandshake::respond(&PSK, &hello, &[2; NONCE_LEN]).unwrap();
    server_hello[4] = 9;
    assert_eq!(
        client.on_server_hello(&server_hello).err(),
        Some(HandshakeError::Version)
    );
}

#[test]
fn a_tampered_record_never_opens() {
    let (mut client, mut server) = pair();
    let record = seal(&mut client, b"change the configuration");
    // Every bit of the header, the data and the tag counts.
    for bit in 0..record.len() * 8 {
        let (mut client, mut server) = pair();
        let record = seal(&mut client, b"change the configuration");
        let mut altered_record = record.clone();
        altered_record[bit / 8] ^= 1 << (bit % 8);
        assert!(
            open(&mut server, &altered_record, 2048).is_err(),
            "bit {bit} of the record"
        );
    }
    assert!(open(&mut server, &record, 2048).is_ok());
}

#[test]
fn a_replayed_reordered_or_dropped_record_does_not_authenticate() {
    let (mut client, mut server) = pair();
    let first = seal(&mut client, b"one");
    let second = seal(&mut client, b"two");
    let third = seal(&mut client, b"three");
    // Out of order.
    assert_eq!(
        open(&mut server, &second, 2048),
        Err(RecordError::Authentication)
    );
    // The failure consumed a counter: the channel is not meant to continue, and the right record
    // after a refused one is refused too (the connection ends at the first failure).
    let (_, mut server) = pair();
    assert_eq!(open(&mut server, &first, 2048).unwrap(), b"one");
    // A replay.
    assert!(open(&mut server, &first, 2048).is_err());
    // A gap: the second record was dropped.
    let (_, mut server) = pair();
    assert_eq!(open(&mut server, &first, 2048).unwrap(), b"one");
    assert!(open(&mut server, &third, 2048).is_err());
}

#[test]
fn a_record_of_another_connection_or_direction_does_not_authenticate() {
    let (mut client, mut server) = pair();
    let request = seal(&mut client, b"request");
    // Reflected back to the client.
    assert!(open(&mut client, &request, 2048).is_err());
    // From a connection with other nonces (same key).
    let (other_client, hello) = ClientHandshake::start(&PSK, &[9; NONCE_LEN]);
    let (other_server, hello_back) =
        ServerHandshake::respond(&PSK, &hello, &[8; NONCE_LEN]).unwrap();
    let (mut other_client, finish) = other_client.on_server_hello(&hello_back).unwrap();
    let mut other_server = other_server.finish(&finish).unwrap();
    let foreign = seal(&mut other_client, b"request");
    assert!(open(&mut server, &foreign, 2048).is_err());
    assert!(open(&mut other_server, &foreign, 2048).is_ok());
}

#[test]
fn a_truncated_or_malformed_record_is_refused() {
    let (client, mut server) = pair();
    let _ = client;
    assert_eq!(
        server.open_header([2, 0, 5], 100),
        Err(RecordError::Version)
    );
    assert_eq!(
        server.open_header([VERSION, 0, 0], 100),
        Err(RecordError::Length)
    );
    assert_eq!(
        server.open_header([VERSION, 0, 101], 100),
        Err(RecordError::Length)
    );
    assert_eq!(server.open_header([VERSION, 0, 100], 100), Ok(100));
    // The data and the header must agree.
    let mut data = [0u8; 4];
    assert_eq!(
        server.open([VERSION, 0, 5], &mut data, &[0; TAG_LEN]),
        Err(RecordError::Authentication)
    );
    let mut empty: [u8; 0] = [];
    assert_eq!(server.seal(&mut empty), Err(RecordError::Length));
}

#[test]
fn an_exhausted_counter_stops_the_channel_for_good() {
    let (mut client, mut server) = pair();
    client.set_counters(u64::MAX - 2, 0);
    server.set_counters(0, u64::MAX - 2);
    // The last two records that may be used.
    for _ in 0..2 {
        let record = seal(&mut client, b"last");
        assert!(open(&mut server, &record, 64).is_ok());
    }
    let mut data = *b"more";
    assert_eq!(client.seal(&mut data), Err(RecordError::Exhausted));
    assert_eq!(client.seal(&mut data), Err(RecordError::Exhausted));
    // The data was not touched by the refused seal.
    assert_eq!(&data, b"more");
    // The receiving side refuses too: u64::MAX is never a counter.
    let header = [VERSION, 0, 4];
    assert_eq!(
        server.open(header, &mut data, &[0; TAG_LEN]),
        Err(RecordError::Exhausted)
    );
}
