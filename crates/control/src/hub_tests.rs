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
    build: "hub-board/1.0.0+00000000.20260101",
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

fn serve<B: crate::ControlBackend, P: crate::AuthorizationPolicy, T>(
    hub: &ControlHub<'_, 4>,
    controller: &Controller<B, P>,
    client: impl core::future::Future<Output = T>,
) -> T {
    match block_on(select(hub.serve(controller), client)) {
        Either::First(never) => never,
        Either::Second(value) => value,
    }
}

// A queued export of a session bound to the current key, then the key rotates (or not) before
// the worker gets to it. Returns whether the response is empty and whether the export ran.
fn queued_export(rotate: bool) -> (bool, bool) {
    let access = ControlAccess::new(|_| false);
    access.set_tcp_psk(true);
    let hub = ControlHub::<4>::with_access(&access);
    let files = crate::tests::files();
    let controller = crate::tests::files_controller(&files);
    let connection = hub.connect(Transport::Tcp).unwrap();
    connection.set_link(true, true);
    connection.set_epoch(access.tcp_psk_epoch());
    let mut reservation = connection.request().unwrap();
    let bytes = request(7, crate::tests::export(1));
    reservation.buffer()[..bytes.len()].copy_from_slice(&bytes);
    let mut submit = core::pin::pin!(reservation.submit(bytes.len()));
    assert!(poll_once(submit.as_mut()).is_pending());
    if rotate {
        access.set_tcp_psk(true);
    }
    let empty = serve(&hub, &controller, async { submit.await.bytes().is_empty() });
    (empty, files.open.get())
}

#[test]
fn a_request_queued_before_the_key_rotated_is_not_executed() {
    assert_eq!(queued_export(false), (false, true));
    // Nothing runs and the empty response ends the connection.
    assert_eq!(queued_export(true), (true, false));
}

#[test]
fn a_request_of_a_session_with_no_key_epoch_is_not_affected_by_rotation() {
    let access = ControlAccess::new(|_| false);
    let hub = ControlHub::<4>::with_access(&access);
    let controller = controller();
    let session = hub.open_session(Transport::Ble).unwrap();
    access.set_tcp_psk(true);
    let reply = run(&hub, &controller, exchange(&hub, session, 1, info()));
    assert!(matches!(reply, Ok(Reply::GetInfo(_))));
}

#[test]
fn the_end_of_a_connection_is_not_lost_when_the_queue_is_full() {
    let access = ControlAccess::new(|_| false);
    let hub = ControlHub::<4>::with_access(&access);
    let files = crate::tests::files();
    let controller = crate::tests::files_controller(&files);
    let connection = hub.connect(Transport::Ble).unwrap();
    let export = serve(&hub, &controller, async {
        exchange(&hub, connection.id(), 1, crate::tests::export(1)).await
    });
    assert!(matches!(export, Ok(Reply::ActionStream(_))));
    assert!(files.open.get());
    // Fill the queue: an ending per dropped connection and one request ticket.
    for _ in 0..7 {
        drop(hub.connect(Transport::Ble).unwrap());
    }
    let mut reservation = connection.request().unwrap();
    let bytes = request(2, info());
    reservation.buffer()[..bytes.len()].copy_from_slice(&bytes);
    {
        let mut submit = core::pin::pin!(reservation.submit(bytes.len()));
        assert!(poll_once(submit.as_mut()).is_pending());
        // No room for the wake-up of this one.
        drop(connection);
    }
    serve(&hub, &controller, async {
        for _ in 0..16 {
            embassy_futures::yield_now().await;
        }
    });
    assert!(
        !files.open.get(),
        "the stream of the ended connection is closed"
    );
}
