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
//! HTTP/1.1 transport over the Rohi IP data plane.
//!
//! ```text
//! POST /v1/telemetry HTTP/1.1
//! Host: <host>[:<port>]
//! Content-Type: application/protobuf
//! Content-Length: <packet length>
//! Connection: close
//!
//! <exact SignedEnvelope bytes>
//! ```
//!
//! The port is part of the `Host` header unless it is 80. The request goes
//! over plain TCP (DNS or an IPv4 literal, then `NetworkHandle::connect`),
//! one request per connection, with a total timeout per attempt
//! ([`DEFAULT_TIMEOUT_MS`], 20 s). The response body is ignored; only the
//! status line is read (see [`classify_status`], the classification table is
//! in [`transport`](crate::transport)). The transport is plain HTTP: TLS can
//! later wrap the connection without touching the message or signature.
//!
//! The timeout is a `select` against [`DelayNs`]: when it fires the exchange
//! future is dropped and the attempt counts as
//! [`TransportError::Timeout`].

use core::fmt::Write as _;
use core::net::{IpAddr, SocketAddr};

use embedded_hal_async::delay::DelayNs;
use embedded_io_async::{Read, Write};
use embedded_nal_async::AddrType;
use heapless::String;
use rohi_net::{IpBackend, NetError, NetworkHandle};
use rohi_time::with_timeout;

use crate::endpoint::Endpoint;
use crate::transport::{PacketTransport, TransportAck, TransportError, classify_status};

/// Path of the telemetry ingress.
pub const TELEMETRY_PATH: &str = "/v1/telemetry";
/// `Content-Type` of the request body.
pub const CONTENT_TYPE: &str = "application/protobuf";
/// Default budget of one delivery attempt (DNS, connect, send, response).
pub const DEFAULT_TIMEOUT_MS: u32 = 20_000;

const HEAD_CAPACITY: usize = 256;
const RESPONSE_CAPACITY: usize = 64;

/// HTTP transport of sealed packets.
///
/// One attempt resolves the host (an IP literal skips DNS), opens a
/// connection through the [`NetworkHandle`], writes the request head and the
/// exact packet bytes and reads the status line; everything is bounded by the
/// timeout of the transport. Only `202` is an acknowledgement, see
/// [`classify_status`].
///
/// The example talks to a scripted server on the in-memory IP backend of
/// `rohi-net` (feature `testing`):
///
/// ```
/// use core::net::{IpAddr, Ipv4Addr, SocketAddr};
///
/// use embassy_futures::{block_on, join::join, yield_now};
/// use embedded_hal_async::delay::DelayNs;
/// use rohi_connectivity::http::request_head;
/// use rohi_connectivity::{Endpoint, HttpTransport, PacketTransport, TransportAck};
/// use rohi_net::NetworkHandle;
/// use rohi_net::testing::FakeIp;
///
/// // A timeout that never fires; the firmware passes its embassy delay.
/// struct NeverDelay;
/// impl DelayNs for NeverDelay {
///     async fn delay_ns(&mut self, _ns: u32) {
///         core::future::pending::<()>().await
///     }
/// }
///
/// // Any sealed packet: the transport sends exactly these bytes.
/// let packet = [0x0a, 0x04, 0x08, 0x07, 0x10, 0x01];
///
/// let server_addr = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7));
/// let ip = FakeIp::<2, 1024>::new();
/// ip.set_ready(true);
/// ip.add_host("ingress.test", server_addr);
///
/// let endpoint = Endpoint::parse("http://ingress.test").unwrap();
/// let head = request_head(endpoint, packet.len()).unwrap();
/// let mut transport = HttpTransport::new(NetworkHandle::new(&ip), NeverDelay);
///
/// // The server: wait for head and packet, answer `202 Accepted`.
/// let remote = SocketAddr::new(server_addr, 80);
/// let server = async {
///     let mut received = Vec::new();
///     while received.len() < head.len() + packet.len() {
///         received.extend_from_slice(&ip.peer_received(remote));
///         yield_now().await;
///     }
///     ip.peer_send(remote, b"HTTP/1.1 202 Accepted\r\n\r\n");
///     ip.peer_close(remote);
///     received
/// };
///
/// let (result, received) = block_on(join(transport.deliver(endpoint, &packet), server));
/// assert_eq!(result, Ok(TransportAck { status: 202 }));
/// assert!(received.starts_with(head.as_bytes()));
/// assert!(received.ends_with(&packet)); // the exact signed bytes
/// ```
pub struct HttpTransport<'a, B, D> {
    net: NetworkHandle<'a, B>,
    delay: D,
    timeout_ms: u32,
}

impl<'a, B: IpBackend, D: DelayNs> HttpTransport<'a, B, D> {
    /// A transport over `net`; `delay` bounds every attempt.
    pub fn new(net: NetworkHandle<'a, B>, delay: D) -> Self {
        Self {
            net,
            delay,
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }

    /// Replaces the budget of one attempt.
    pub fn with_timeout_ms(mut self, timeout_ms: u32) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }

    async fn exchange(
        net: NetworkHandle<'a, B>,
        endpoint: Endpoint<'_>,
        packet: &[u8],
    ) -> Result<u16, TransportError> {
        let address = match endpoint.host.parse::<IpAddr>() {
            Ok(address) => address,
            Err(_) => net
                .resolve(endpoint.host, AddrType::IPv4)
                .await
                .map_err(map_error)?,
        };
        let mut connection = net
            .connect(SocketAddr::new(address, endpoint.port))
            .await
            .map_err(map_error)?;

        let head = request_head(endpoint, packet.len())?;
        connection
            .write_all(head.as_bytes())
            .await
            .map_err(map_error)?;
        connection.write_all(packet).await.map_err(map_error)?;
        connection.flush().await.map_err(map_error)?;

        let mut response = [0u8; RESPONSE_CAPACITY];
        let mut filled = 0;
        loop {
            if let Some(status) = parse_status(&response[..filled])? {
                return Ok(status);
            }
            if filled == response.len() {
                return Err(TransportError::BadResponse);
            }
            match connection.read(&mut response[filled..]).await {
                Ok(0) => return Err(TransportError::Io),
                Ok(read) => filled += read,
                Err(error) => return Err(map_error(error)),
            }
        }
    }
}

impl<B: IpBackend, D: DelayNs> PacketTransport for HttpTransport<'_, B, D> {
    async fn deliver(
        &mut self,
        endpoint: Endpoint<'_>,
        packet: &[u8],
    ) -> Result<TransportAck, TransportError> {
        let exchange = Self::exchange(self.net, endpoint, packet);
        let status = with_timeout(&mut self.delay, self.timeout_ms, exchange)
            .await
            .map_err(|_| TransportError::Timeout)??;
        classify_status(status)
    }
}

fn map_error(error: NetError) -> TransportError {
    match error {
        NetError::NetworkUnavailable => TransportError::NetworkUnavailable,
        NetError::DnsFailed => TransportError::Dns,
        NetError::ConnectFailed | NetError::NoSocketAvailable => TransportError::Connect,
        NetError::Timeout => TransportError::Timeout,
        NetError::ConnectionReset | NetError::Backend => TransportError::Io,
    }
}

/// The request line and headers for a body of `content_length` bytes.
///
/// # Errors
///
/// [`TransportError::BadResponse`] if the head does not fit its 256-byte
/// buffer (a host and port that the endpoint parser would not accept).
///
/// ```
/// use rohi_connectivity::Endpoint;
/// use rohi_connectivity::http::request_head;
///
/// let endpoint = Endpoint::parse("http://ingress.example.org:8080").unwrap();
/// assert_eq!(
///     request_head(endpoint, 194).unwrap().as_str(),
///     "POST /v1/telemetry HTTP/1.1\r\n\
///      Host: ingress.example.org:8080\r\n\
///      Content-Type: application/protobuf\r\n\
///      Content-Length: 194\r\n\
///      Connection: close\r\n\r\n"
/// );
/// ```
pub fn request_head(
    endpoint: Endpoint<'_>,
    content_length: usize,
) -> Result<String<HEAD_CAPACITY>, TransportError> {
    let mut head = String::new();
    let host_header = if endpoint.port == 80 {
        write!(
            head,
            "POST {TELEMETRY_PATH} HTTP/1.1\r\nHost: {}\r\n",
            endpoint.host
        )
    } else {
        write!(
            head,
            "POST {TELEMETRY_PATH} HTTP/1.1\r\nHost: {}:{}\r\n",
            endpoint.host, endpoint.port
        )
    };
    host_header
        .and_then(|()| {
            write!(
                head,
                "Content-Type: {CONTENT_TYPE}\r\nContent-Length: {content_length}\r\nConnection: close\r\n\r\n"
            )
        })
        .map_err(|_| TransportError::BadResponse)?;
    Ok(head)
}

/// The status code once the status line is complete, `None` if more bytes
/// are needed.
fn parse_status(bytes: &[u8]) -> Result<Option<u16>, TransportError> {
    let Some(end) = bytes.windows(2).position(|pair| pair == b"\r\n") else {
        return Ok(None);
    };
    let line = &bytes[..end];
    if line.len() < 12 || !line.starts_with(b"HTTP/1.") || line[8] != b' ' {
        return Err(TransportError::BadResponse);
    }
    let digits = &line[9..12];
    if !digits.iter().all(u8::is_ascii_digit) || line.get(12).is_some_and(|b| *b != b' ') {
        return Err(TransportError::BadResponse);
    }
    let status = digits
        .iter()
        .fold(0u16, |acc, digit| acc * 10 + u16::from(digit - b'0'));
    Ok(Some(status))
}

#[cfg(test)]
mod tests;
