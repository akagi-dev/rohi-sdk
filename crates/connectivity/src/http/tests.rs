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
use core::net::{IpAddr, Ipv4Addr, SocketAddr};

use embassy_futures::{block_on, join::join, yield_now};
use embedded_hal_async::delay::DelayNs;
use rohi_identity::MockSigner;
use rohi_net::NetworkHandle;
use rohi_net::testing::FakeIp;

use super::*;
use crate::tests::{CountingNonce, NoDelay};
use crate::{Disposition, EnvelopeBuilder, EnvelopeFrame};

const SERVER: IpAddr = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7));
const HOST: &str = "ingress.test";

type Ip = FakeIp<2, 1024>;

/// A timeout that never fires.
struct NeverDelay;

impl DelayNs for NeverDelay {
    async fn delay_ns(&mut self, _ns: u32) {
        core::future::pending::<()>().await
    }
}

fn frame() -> EnvelopeFrame {
    let signer = MockSigner::default();
    let mut nonce = CountingNonce::default();
    EnvelopeBuilder::new(&signer, &mut nonce)
        .sign(&[0x0a, 0x04, 0x08, 0x07, 0x10, 0x01])
        .unwrap()
}

fn ip() -> Ip {
    let ip = Ip::new();
    ip.set_ready(true);
    ip.add_host(HOST, SERVER);
    ip
}

/// The server: waits for the whole request, answers with `response` and
/// returns what it received.
async fn server(ip: &Ip, port: u16, expected: usize, response: &[u8]) -> std::vec::Vec<u8> {
    let remote = SocketAddr::new(SERVER, port);
    let mut received = std::vec::Vec::new();
    while received.len() < expected {
        received.extend_from_slice(&ip.peer_received(remote));
        yield_now().await;
    }
    ip.peer_send(remote, response);
    ip.peer_close(remote);
    received
}

fn endpoint(port: u16) -> Endpoint<'static> {
    Endpoint { host: HOST, port }
}

fn exchange(
    ip: &Ip,
    endpoint: Endpoint<'static>,
    response: &[u8],
) -> (Result<TransportAck, TransportError>, std::vec::Vec<u8>) {
    let frame = frame();
    let head = request_head(endpoint, frame.len()).unwrap();
    let mut transport = HttpTransport::new(NetworkHandle::new(ip), endpoint, NeverDelay);
    let (result, received) = block_on(join(
        transport.send(&frame),
        server(ip, endpoint.port, head.len() + frame.len(), response),
    ));
    let mut expected = std::vec::Vec::from(head.as_bytes());
    expected.extend_from_slice(frame.as_bytes());
    assert_eq!(received, expected, "head and the exact frame bytes");
    (result, received)
}

#[test]
fn request_shape_is_exactly_the_documented_one() {
    let head = request_head(endpoint(80), 123).unwrap();
    assert_eq!(
        head.as_str(),
        "POST /v1/telemetry HTTP/1.1\r\n\
         Host: ingress.test\r\n\
         Content-Type: application/protobuf\r\n\
         Content-Length: 123\r\n\
         Connection: close\r\n\r\n"
    );
    let head = request_head(endpoint(8080), 5).unwrap();
    assert!(head.contains("Host: ingress.test:8080\r\n"));
}

#[test]
fn accepted_frame_is_sent_byte_for_byte_and_acknowledged() {
    let ip = ip();
    let (result, _) = exchange(
        &ip,
        endpoint(80),
        b"HTTP/1.1 202 Accepted\r\nContent-Length: 8\r\n\r\naccepted",
    );
    assert_eq!(result, Ok(TransportAck { status: 202 }));
    assert_eq!(ip.open_connections(), 0, "the connection slot is released");
}

#[test]
fn a_non_default_port_is_connected_to() {
    let ip = ip();
    let (result, _) = exchange(&ip, endpoint(8080), b"HTTP/1.1 202 Accepted\r\n\r\n");
    assert_eq!(result, Ok(TransportAck { status: 202 }));
}

#[test]
fn server_statuses_become_classified_errors() {
    for (status, disposition) in [
        (
            b"HTTP/1.1 503 Service Unavailable\r\n\r\n".as_slice(),
            Disposition::Retry,
        ),
        (
            b"HTTP/1.1 429 Too Many Requests\r\n\r\n",
            Disposition::Retry,
        ),
        (
            b"HTTP/1.1 401 Unauthorized\r\n\r\n",
            Disposition::Unauthorized,
        ),
        (b"HTTP/1.1 400 Bad Request\r\n\r\n", Disposition::Permanent),
        (
            b"HTTP/1.0 413 Payload Too Large\r\n\r\n",
            Disposition::Permanent,
        ),
    ] {
        let ip = ip();
        let (result, _) = exchange(&ip, endpoint(80), status);
        assert_eq!(result.unwrap_err().disposition(), disposition);
    }
}

#[test]
fn an_ip_literal_needs_no_dns() {
    let ip = ip();
    let endpoint = Endpoint {
        host: "203.0.113.7",
        port: 80,
    };
    let (result, _) = exchange(&ip, endpoint, b"HTTP/1.1 202 Accepted\r\n\r\n");
    assert!(result.is_ok());
    assert_eq!(ip.dns_queries(), 0);
}

fn send_with<D: DelayNs>(ip: &Ip, delay: D) -> Result<TransportAck, TransportError> {
    let mut transport = HttpTransport::new(NetworkHandle::new(ip), endpoint(80), delay);
    block_on(transport.send(&frame()))
}

#[test]
fn network_and_connection_failures_are_retryable() {
    let down = Ip::new();
    assert_eq!(
        send_with(&down, NeverDelay),
        Err(TransportError::NetworkUnavailable)
    );

    let unknown = Ip::new();
    unknown.set_ready(true);
    assert_eq!(send_with(&unknown, NeverDelay), Err(TransportError::Dns));

    let refused = ip();
    refused.refuse(SocketAddr::new(SERVER, 80));
    assert_eq!(
        send_with(&refused, NeverDelay),
        Err(TransportError::Connect)
    );

    let stalled = ip();
    stalled.hold_connects(true);
    assert_eq!(send_with(&stalled, NoDelay), Err(TransportError::Timeout));

    for error in [
        TransportError::NetworkUnavailable,
        TransportError::Dns,
        TransportError::Connect,
        TransportError::Timeout,
    ] {
        assert_eq!(error.disposition(), Disposition::Retry);
    }
}

#[test]
fn closing_before_a_response_is_a_retryable_io_error() {
    let ip = ip();
    let frame = frame();
    let endpoint = endpoint(80);
    let head = request_head(endpoint, frame.len()).unwrap();
    let mut transport = HttpTransport::new(NetworkHandle::new(&ip), endpoint, NeverDelay);
    let (result, _) = block_on(join(
        transport.send(&frame),
        server(&ip, 80, head.len() + frame.len(), b""),
    ));
    assert_eq!(result, Err(TransportError::Io));
}

#[test]
fn a_malformed_status_line_is_a_bad_response() {
    for response in [
        b"garbage\r\n".as_slice(),
        b"HTTP/1.1 20X OK\r\n",
        b"HTTP/2 200\r\n",
        b"HTTP/1.1 2020 OK\r\n",
    ] {
        let ip = ip();
        let (result, _) = exchange(&ip, endpoint(80), response);
        assert_eq!(result, Err(TransportError::BadResponse), "{response:?}");
    }
}

#[test]
fn the_status_line_may_arrive_in_pieces() {
    assert_eq!(parse_status(b""), Ok(None));
    assert_eq!(parse_status(b"HTTP/1.1 20"), Ok(None));
    assert_eq!(parse_status(b"HTTP/1.1 202\r\n"), Ok(Some(202)));
    assert_eq!(
        parse_status(b"HTTP/1.1 202 Accepted\r\nX: y"),
        Ok(Some(202))
    );
}
