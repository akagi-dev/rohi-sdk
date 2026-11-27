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
//! A route whose primary is Meshtastic and whose fallback is HTTP, driven through the real
//! router and both real transports behind one static `Mux`.

use core::cell::RefCell;
use core::net::{IpAddr, Ipv4Addr, SocketAddr};

use embassy_futures::{block_on, join::join, yield_now};
use embedded_hal_async::delay::DelayNs;
use rohi_connectivity::{
    AckKind, EndpointPolicy, Mux, PacketHandle, PacketTransport, Router, Routes, TransportError,
};
use rohi_connectivity_http::{HttpPolicy, HttpTransport, request_head};
use rohi_connectivity_meshtastic::{Link, MeshtasticPolicy, MeshtasticTransport};
use rohi_meshtastic::{MeshtasticRadio, Outgoing, RadioError, RoutingResult};
use rohi_net::NetworkHandle;
use rohi_net::testing::FakeIp;
use rohi_telemetry::ProfileStamp;
use rohi_time::MonotonicTimestamp;

const SERVER: IpAddr = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7));
const PRIMARY: &str = "meshtastic+uart://!deadbeef";
const FALLBACK: &str = "http://ingress.test";

type Ip = FakeIp<2, 8192>;
type Policy = Mux<HttpPolicy, MeshtasticPolicy>;
const POLICY: Policy = Mux(HttpPolicy, MeshtasticPolicy::UART);

struct NeverDelay;

impl DelayNs for NeverDelay {
    async fn delay_ns(&mut self, _ns: u32) {
        core::future::pending::<()>().await
    }
}

/// A radio that acknowledges every packet it is given.
struct AckingRadio<'a> {
    sent: &'a RefCell<Vec<Vec<u8>>>,
    acks: RefCell<Vec<u32>>,
    next: u32,
}

impl MeshtasticRadio for AckingRadio<'_> {
    fn reserve_ids(&mut self, count: usize) -> u32 {
        let first = self.next;
        self.next += count as u32;
        first
    }

    async fn send(&mut self, packet: &Outgoing<'_>) -> Result<(), RadioError> {
        self.sent.borrow_mut().push(packet.payload.to_vec());
        self.acks.borrow_mut().push(packet.id);
        Ok(())
    }

    async fn next_result(&mut self) -> Result<RoutingResult, RadioError> {
        loop {
            if let Some(id) = self.acks.borrow_mut().pop() {
                return Ok(RoutingResult {
                    request_id: id,
                    error: 0,
                });
            }
            yield_now().await;
        }
    }
}

fn t(ms: u64) -> MonotonicTimestamp {
    MonotonicTimestamp::from_millis(ms)
}

fn queue(router: &mut Router<2, 2, 4, 4096>, bytes: &[u8], now: u64) {
    let stamp = ProfileStamp {
        profile_index: 0,
        revision: 1,
    };
    let (handle, buffer): (PacketHandle, _) = router.pool_mut().alloc(stamp).unwrap();
    buffer[..bytes.len()].copy_from_slice(bytes);
    assert!(router.pool_mut().seal(handle, bytes.len()));
    router.admit(0, handle, t(now));
    router.pool_mut().release(handle);
}

#[test]
fn an_oversize_packet_goes_to_http_and_a_small_one_over_the_mesh_with_the_same_bytes() {
    let mut config = Routes::<2>::default();
    config.set_endpoint(0, 0, Some(PRIMARY), &POLICY).unwrap();
    config.set_fallback(0, Some(FALLBACK), &POLICY).unwrap();
    assert_eq!(config.validate(&POLICY), Ok(()));
    // The same route is refused by a build without the Meshtastic backend.
    assert!(config.validate(&HttpPolicy).is_err());

    let mesh_sent = RefCell::new(Vec::new());
    let ip = Ip::new();
    ip.set_ready(true);
    ip.add_host("ingress.test", SERVER);
    let mesh = MeshtasticTransport::new(
        Link::Uart,
        AckingRadio {
            sent: &mesh_sent,
            acks: RefCell::new(Vec::new()),
            next: 1,
        },
        NeverDelay,
    );
    let http = HttpTransport::new(NetworkHandle::new(&ip), NeverDelay);
    let mut transport = Mux(http, mesh);

    let mut router = Router::<2, 2, 4, 4096>::new();
    router.sync(&config, |_| true, t(0));

    // 3500 bytes cannot be framed for the mesh: the path is incompatible, HTTP carries it.
    let big: Vec<u8> = (0..3_500u32).map(|i| (i % 251) as u8).collect();
    queue(&mut router, &big, 0);
    let first = router.next_delivery(&config, t(0)).unwrap();
    assert_eq!(first.path, 0);
    let uri = |path: usize| if path == 0 { PRIMARY } else { FALLBACK };
    let bytes = router.pool().get(first.packet).unwrap().1.to_vec();
    let result = block_on(transport.deliver(uri(first.path), &bytes));
    assert_eq!(result, Err(TransportError::Incompatible));
    router.complete(first, result, t(0));
    assert!(
        mesh_sent.borrow().is_empty(),
        "nothing of it went to the radio"
    );

    let second = router.next_delivery(&config, t(0)).unwrap();
    assert_eq!(
        second.path, 1,
        "the fallback at once, without a cooldown of the primary"
    );
    let head = request_head(
        rohi_connectivity_http::Endpoint::parse(FALLBACK).unwrap(),
        big.len(),
        None,
    )
    .unwrap();
    let remote = SocketAddr::new(SERVER, 80);
    let server = async {
        let mut received = Vec::new();
        while received.len() < head.len() + big.len() {
            received.extend_from_slice(&ip.peer_received(remote));
            yield_now().await;
        }
        ip.peer_send(remote, b"HTTP/1.1 202 Accepted\r\n\r\n");
        ip.peer_close(remote);
        received
    };
    let (result, received) = block_on(join(transport.deliver(uri(second.path), &bytes), server));
    let ack = result.unwrap();
    assert_eq!(ack.kind, AckKind::Accepted);
    assert!(received.ends_with(&big), "byte-identical over the fallback");
    router.complete(second, Ok(ack), t(0));
    assert_eq!(router.queued(), 0);

    // The next, small packet is not affected by the earlier incompatibility: mesh again.
    let small: Vec<u8> = (0..300u32).map(|i| (i % 7) as u8).collect();
    queue(&mut router, &small, 60_000);
    let delivery = router.next_delivery(&config, t(60_000)).unwrap();
    assert_eq!(delivery.path, 0);
    let bytes = router.pool().get(delivery.packet).unwrap().1.to_vec();
    let result = block_on(transport.deliver(uri(0), &bytes));
    let ack = result.unwrap();
    assert_eq!((ack.kind, ack.detail), (AckKind::Relayed, 2));
    router.complete(delivery, Ok(ack), t(60_000));
    let mut rebuilt = Vec::new();
    for frame in mesh_sent.borrow().iter() {
        rebuilt.extend_from_slice(&frame[8..]);
    }
    assert_eq!(rebuilt, small, "the sealed bytes, fragmented and unchanged");
    let view = router.views(&config, t(60_000))[0].unwrap();
    assert_eq!(
        (
            view.counters.delivered,
            view.counters.incompatible,
            view.counters.retries
        ),
        (2, 0, 0)
    );
}

#[test]
fn a_packet_no_path_can_carry_is_dropped_once() {
    let mut config = Routes::<2>::default();
    config.set_endpoint(0, 0, Some(PRIMARY), &POLICY).unwrap();
    config
        .set_fallback(0, Some("meshtastic+uart://!00000001"), &POLICY)
        .unwrap();
    let mesh_sent = RefCell::new(Vec::new());
    let mut transport = MeshtasticTransport::new(
        Link::Uart,
        AckingRadio {
            sent: &mesh_sent,
            acks: RefCell::new(Vec::new()),
            next: 1,
        },
        NeverDelay,
    );
    let mut router = Router::<2, 2, 4, 4096>::new();
    router.sync(&config, |_| true, t(0));
    queue(&mut router, &vec![1u8; 3_400], 0);
    while let Some(delivery) = router.next_delivery(&config, t(0)) {
        let bytes = router.pool().get(delivery.packet).unwrap().1.to_vec();
        let uri = if delivery.path == 0 {
            PRIMARY
        } else {
            "meshtastic+uart://!00000001"
        };
        let result = block_on(transport.deliver(uri, &bytes));
        router.complete(delivery, result, t(0));
    }
    assert_eq!(router.queued(), 0);
    let counters = router.views(&config, t(0))[0].unwrap().counters;
    assert_eq!(
        (counters.incompatible, counters.delivered, counters.retries),
        (1, 0, 0)
    );
    assert!(mesh_sent.borrow().is_empty());
}

#[test]
fn the_policies_compose_without_dyn() {
    assert!(POLICY.validate("http://a.example").is_ok());
    assert!(POLICY.validate(PRIMARY).is_ok());
    assert!(
        POLICY.validate("meshtastic+ble://!deadbeef").is_err(),
        "no BLE backend in this build"
    );
    assert!(POLICY.equivalent("http://A.example", "http://a.example:80"));
    assert!(POLICY.equivalent("meshtastic+uart://!DEADBEEF", PRIMARY));
}
