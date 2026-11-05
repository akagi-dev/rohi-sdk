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

//! The bridge between BlueZ callbacks and the transport, without BlueZ.

use embassy_futures::block_on;
use embassy_futures::select::{Either, select};
use rohi_control::{Controller, Device, DeviceInfo, OpenPolicy};
use rohi_control_ble::ControlBle;
use rohi_control_ble::frame::{Fragmenter, Reassembler};
use rohi_control_proto::control::{
    self as pb, Request_::Request as Call, Response_::Response as Reply,
};
use rohi_control_proto::{PROTOCOL_VERSION, decode, encode};
use rohi_net::BleGattSpec;

use super::*;

fn config(name: Option<&str>) -> BleConfig {
    BleConfig {
        name_prefix: "ALTRUIST-URBAN",
        name: name.and_then(BleName::new),
        gatt: BleGattSpec {
            service: 1,
            rx: 2,
            tx: 3,
        },
    }
}

#[test]
fn the_default_name_ends_with_the_last_two_address_bytes() {
    let name = advertised_name(&config(None), [0, 1, 2, 3, 0xA1, 0xB2]).unwrap();
    assert_eq!(name.as_str(), "ALTRUIST-URBAN-A1B2");
}

#[test]
fn a_long_prefix_is_cut_to_fit_the_name() {
    let mut config = config(None);
    config.name_prefix = "A-VERY-LONG-PRODUCT-NAME-THAT-DOES-NOT-FIT";
    let name = advertised_name(&config, [0, 0, 0, 0, 0x0F, 0x01]).unwrap();
    assert_eq!(name.as_str().len(), rohi_net::MAX_BLE_NAME_LEN);
    assert!(name.as_str().ends_with("-0F01"));
}

#[test]
fn a_configured_name_replaces_the_default() {
    let name = advertised_name(&config(Some("kitchen")), [0; 6]).unwrap();
    assert_eq!(name.as_str(), "kitchen");
}

#[test]
fn the_link_starts_disabled_and_a_central_makes_it_connected() {
    let (mut transport, peripheral) = bridge();
    let probe = transport.probe();
    assert_eq!(probe.state(), BleState::Disabled);
    assert_eq!(transport.connection_info(), None);
    assert_eq!(transport.connection_epoch(), 0);

    peripheral.set_state(BleState::Advertising);
    assert_eq!(transport.state(), BleState::Advertising);

    peripheral.connected();
    assert_eq!(transport.connection_epoch(), 1);
    assert_eq!(
        transport.connection_info(),
        Some(BleConnectionInfo::default())
    );
    // Further activity of the same central is the same connection.
    peripheral.connected();
    assert_eq!(transport.connection_epoch(), 1);

    block_on(peripheral.disconnected());
    assert_eq!(probe.state(), BleState::Advertising);
    assert_eq!(
        block_on(transport.recv(&mut [0u8; 4])),
        Err(BleError::Disconnected)
    );

    // Reconnection is a new connection.
    peripheral.connected();
    assert_eq!(transport.connection_epoch(), 2);
}

#[test]
fn the_payload_follows_the_mtu_within_the_limits() {
    let (transport, peripheral) = bridge();
    peripheral.connected();
    assert_eq!(transport.max_payload(), DEFAULT_BLE_PAYLOAD);
    peripheral.mtu(100);
    assert_eq!(transport.max_payload(), 97);
    peripheral.mtu(517);
    assert_eq!(transport.max_payload(), MAX_BLE_PAYLOAD);
    peripheral.mtu(10);
    assert_eq!(transport.max_payload(), DEFAULT_BLE_PAYLOAD);
    // A new connection starts from the default again.
    block_on(peripheral.disconnected());
    peripheral.connected();
    assert_eq!(transport.max_payload(), DEFAULT_BLE_PAYLOAD);
}

#[test]
fn a_disconnect_is_reported_once_and_only_for_a_connection() {
    let (mut transport, peripheral) = bridge();
    block_on(peripheral.disconnected());
    peripheral.connected();
    block_on(peripheral.disconnected());
    block_on(peripheral.disconnected());
    let mut out = [0u8; 4];
    assert_eq!(
        block_on(transport.recv(&mut out)),
        Err(BleError::Disconnected)
    );
    // Nothing else is pending: the next receive waits.
    let waited = block_on(select(transport.recv(&mut out), async {}));
    assert!(matches!(waited, Either::Second(())));
}

#[test]
fn chunks_cross_the_bridge_in_both_directions() {
    let (mut transport, peripheral) = bridge();
    peripheral.connected();

    block_on(peripheral.received(b"abc".to_vec()));
    let mut out = [0u8; 8];
    assert_eq!(block_on(transport.recv(&mut out)), Ok(3));
    assert_eq!(&out[..3], b"abc");

    block_on(transport.send(b"xyz")).unwrap();
    let outbox = peripheral.outbox.clone();
    let sent = outbox.try_lock().unwrap().try_recv().unwrap();
    assert_eq!(sent, b"xyz");

    // A chunk above the payload is refused; so is a receive buffer that is
    // too small.
    assert_eq!(
        block_on(transport.send(&[0u8; DEFAULT_BLE_PAYLOAD + 1])),
        Err(BleError::BufferTooSmall)
    );
    block_on(peripheral.received(b"too long".to_vec()));
    assert_eq!(
        block_on(transport.recv(&mut [0u8; 2])),
        Err(BleError::BufferTooSmall)
    );
}

#[test]
fn nothing_is_sent_without_a_connection() {
    let (mut transport, _peripheral) = bridge();
    assert_eq!(block_on(transport.send(b"x")), Err(BleError::Disconnected));
}

/// A central that writes a request in fragments of the link payload and
/// collects the notified fragments of the response.
#[test]
fn the_control_protocol_runs_over_the_bridge() {
    let (transport, peripheral) = bridge();
    let probe = transport.probe();
    peripheral.connected();

    let request = pb::Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: 9,
        request: Some(Call::GetInfo(pb::GetInfoRequest {})),
    };
    let mut bytes = [0u8; 16];
    let len = encode(&request, &mut bytes).unwrap();
    let payload = probe.max_payload();
    let mut chunk = vec![0u8; payload];
    let mut fragments = Fragmenter::new(4, &bytes[..len], payload).unwrap();
    while let Some(n) = fragments.next_frame(&mut chunk) {
        block_on(peripheral.received(chunk[..n].to_vec()));
    }

    let info = DeviceInfo {
        firmware: "demo-0.1.0",
        board: "demo-board",
    };
    let controller = Controller::new(Device::new(info, || 0, (), (), ()), OpenPolicy);
    let mut control = ControlBle::new(transport, &controller);
    // Every fragment is one step; the last one answers.
    for _ in 0..fragments.count() {
        block_on(control.step());
    }

    let mut response = Reassembler::<256>::new();
    let mut message = None;
    let outbox = peripheral.outbox.clone();
    let mut outbox = outbox.try_lock().unwrap();
    while let Ok(frame) = outbox.try_recv() {
        assert!(frame.len() <= payload);
        if let Some(done) = response.push(&frame).unwrap() {
            assert_eq!(done.id, 4);
            message = Some(decode::<pb::Response>(done.bytes).unwrap());
        }
    }
    let message = message.expect("a complete response");
    assert_eq!(message.request_id, 9);
    let Some(Reply::GetInfo(info)) = message.response else {
        panic!("not a GetInfo response");
    };
    assert_eq!(info.board.as_str(), "demo-board");
}

#[test]
fn a_response_to_a_connection_that_ended_is_dropped() {
    let (transport, peripheral) = bridge();
    peripheral.connected();
    let request = pb::Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: 1,
        request: Some(Call::GetInfo(pb::GetInfoRequest {})),
    };
    let mut bytes = [0u8; 16];
    let len = encode(&request, &mut bytes).unwrap();
    let mut chunk = [0u8; MAX_BLE_PAYLOAD];
    let mut fragments = Fragmenter::new(1, &bytes[..len], MAX_BLE_PAYLOAD).unwrap();
    let n = fragments.next_frame(&mut chunk).unwrap();
    block_on(peripheral.received(chunk[..n].to_vec()));
    // The central goes away before the device answers.
    block_on(peripheral.disconnected());

    let info = DeviceInfo {
        firmware: "demo-0.1.0",
        board: "demo-board",
    };
    let controller = Controller::new(Device::new(info, || 0, (), (), ()), OpenPolicy);
    let mut control = ControlBle::new(transport, &controller);
    block_on(control.step());
    assert!(peripheral.outbox.try_lock().unwrap().try_recv().is_err());
}
