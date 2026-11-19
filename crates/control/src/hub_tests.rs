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
use embassy_futures::block_on;
use embassy_futures::poll_once;
use embassy_futures::select::{Either, select};
use rohi_control_proto::control::{
    self as pb, Request_::Request as Call, Response_::Response as Reply,
};
use rohi_control_proto::{PROTOCOL_VERSION, decode, encode, limits};

use crate::{
    Busy, ControlAccess, ControlHub, Controller, Device, DeviceInfo, RequireEncryption, Transport,
};

const INFO: DeviceInfo = DeviceInfo {
    firmware: "hub-0.1.0",
    board: "hub-board",
};

fn controller() -> Controller<Device<(), (), ()>, RequireEncryption> {
    Controller::new(Device::new(INFO, || 0, (), (), ()), RequireEncryption)
}

fn request(id: u32, call: Call) -> std::vec::Vec<u8> {
    let message = pb::Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: id,
        request: Some(call),
    };
    let mut wire = [0u8; limits::MAX_REQUEST_LEN];
    let len = encode(&message, &mut wire).unwrap();
    wire[..len].to_vec()
}

fn info() -> Call {
    Call::GetInfo(pb::GetInfoRequest {})
}

fn mutation() -> Call {
    Call::ResetConfigSection(pb::ResetConfigSectionRequest::default())
}

async fn exchange<const N: usize>(
    hub: &ControlHub<'_, N>,
    session: crate::SessionId,
    id: u32,
    call: Call,
) -> Result<Reply, Busy> {
    let mut reservation = hub.request(session)?;
    let bytes = request(id, call);
    reservation.buffer()[..bytes.len()].copy_from_slice(&bytes);
    let response = reservation.submit(bytes.len()).await;
    let decoded: pb::Response = decode(response.bytes()).unwrap();
    assert_eq!(decoded.request_id, id);
    Ok(decoded.response.unwrap())
}

fn run<T, const N: usize>(
    hub: &ControlHub<'_, N>,
    controller: &Controller<Device<(), (), ()>, RequireEncryption>,
    client: impl core::future::Future<Output = T>,
) -> T {
    match block_on(select(hub.serve(controller), client)) {
        Either::First(never) => never,
        Either::Second(value) => value,
    }
}

#[test]
fn a_request_goes_through_the_one_worker_and_the_response_comes_back() {
    let access = ControlAccess::new(|_| false);
    let hub = ControlHub::<2>::with_access(&access);
    let controller = controller();
    let session = hub.open_session(Transport::Tcp).unwrap();
    let reply = run(&hub, &controller, exchange(&hub, session, 5, info()));
    assert!(matches!(reply, Ok(Reply::GetInfo(_))));
    // The slot is free again for the next request.
    let reply = run(&hub, &controller, exchange(&hub, session, 6, info()));
    assert!(matches!(reply, Ok(Reply::GetInfo(_))));
}

#[test]
fn sessions_are_unique_across_transports_and_bounded() {
    let access = ControlAccess::new(|_| false);
    let hub = ControlHub::<1>::with_access(&access);
    let ble = hub.open_session(Transport::Ble).unwrap();
    let tcp = hub.open_session(Transport::Tcp).unwrap();
    assert_ne!(ble, tcp);
    assert_eq!(access.stats(Transport::Ble).sessions, 1);
    assert_eq!(access.stats(Transport::Tcp).sessions, 1);
    let more: std::vec::Vec<_> = (0..crate::MAX_SESSIONS)
        .map(|_| hub.open_session(Transport::Tcp))
        .collect();
    assert!(more.iter().any(|session| session == &Err(Busy::Busy)));
    let controller = controller();
    run(&hub, &controller, hub.close_session(tcp));
    assert_eq!(
        access.stats(Transport::Tcp).sessions as usize,
        more.iter().filter(|s| s.is_ok()).count()
    );
    // A closed session is gone and its identity is never reused.
    assert_eq!(hub.request(tcp).err(), Some(Busy::Closed));
    let fresh = hub.open_session(Transport::Tcp).unwrap();
    assert_ne!(fresh, tcp);
}

#[test]
fn a_connection_has_one_request_in_flight_and_the_slots_are_bounded() {
    let access = ControlAccess::new(|_| false);
    let hub = ControlHub::<1>::with_access(&access);
    let first = hub.open_session(Transport::Tcp).unwrap();
    let second = hub.open_session(Transport::Ble).unwrap();
    let held = hub.request(first).unwrap();
    assert_eq!(hub.request(first).err(), Some(Busy::Busy));
    // Every slot is taken.
    assert_eq!(hub.request(second).err(), Some(Busy::Busy));
    drop(held);
    assert!(hub.request(second).is_ok());
}

#[test]
fn the_link_security_of_a_session_is_the_one_its_transport_reported() {
    let access = ControlAccess::new(|_| false);
    let hub = ControlHub::<2>::with_access(&access);
    let controller = controller();
    let plain = hub.open_session(Transport::Ble).unwrap();
    let paired = hub.open_session(Transport::Ble).unwrap();
    hub.set_link(paired, true, false);
    let denied = run(&hub, &controller, exchange(&hub, plain, 1, mutation()));
    let Ok(Reply::Error(error)) = denied else {
        panic!()
    };
    assert_eq!(error.code, pb::ErrorCode::PermissionDenied);
    let allowed = run(&hub, &controller, exchange(&hub, paired, 1, mutation()));
    let Ok(Reply::Error(error)) = allowed else {
        panic!()
    };
    assert_eq!(error.code, pb::ErrorCode::NotFound);
}

#[test]
fn an_abandoned_request_is_not_executed_and_its_slot_comes_back_wiped() {
    let access = ControlAccess::new(|_| false);
    let hub = ControlHub::<1>::with_access(&access);
    let controller = controller();
    let session = hub.open_session(Transport::Tcp).unwrap();
    {
        let mut reservation = hub.request(session).unwrap();
        let bytes = request(9, info());
        reservation.buffer()[..bytes.len()].copy_from_slice(&bytes);
        let mut submit = core::pin::pin!(reservation.submit(bytes.len()));
        // Queued, then dropped: the client went away.
        assert!(poll_once(submit.as_mut()).is_pending());
    }
    // The slot is not free until the worker has seen the ticket.
    assert_eq!(hub.request(session).err(), Some(Busy::Busy));
    let reply = run(&hub, &controller, async {
        loop {
            match hub.request(session) {
                Ok(mut reservation) => {
                    assert!(reservation.buffer().iter().all(|byte| *byte == 0));
                    break;
                }
                Err(_) => embassy_futures::yield_now().await,
            }
        }
        exchange(&hub, session, 10, info()).await
    });
    assert!(matches!(reply, Ok(Reply::GetInfo(_))));
}

#[test]
fn a_session_that_ended_before_its_request_ran_gets_no_execution() {
    let access = ControlAccess::new(|_| false);
    let hub = ControlHub::<1>::with_access(&access);
    let controller = controller();
    let session = hub.open_session(Transport::Tcp).unwrap();
    let mut reservation = hub.request(session).unwrap();
    let bytes = request(3, mutation());
    reservation.buffer()[..bytes.len()].copy_from_slice(&bytes);
    {
        let mut submit = core::pin::pin!(reservation.submit(bytes.len()));
        assert!(poll_once(submit.as_mut()).is_pending());
        block_on(hub.close_session(session));
    }
    let other = hub.open_session(Transport::Tcp).unwrap();
    let reply = run(&hub, &controller, async {
        loop {
            if hub.request(other).is_ok() {
                break;
            }
            embassy_futures::yield_now().await;
        }
        exchange(&hub, other, 4, info()).await
    });
    assert!(matches!(reply, Ok(Reply::GetInfo(_))));
}
