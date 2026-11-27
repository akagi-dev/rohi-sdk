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
//! User-Agent: <user agent>
//! Connection: close
//!
//! <exact SignedEnvelope bytes>
//! ```
//!
//! The port is part of the `Host` header unless it is 80. `User-Agent` is
//! optional: it is sent only after [`HttpTransport::with_user_agent`]. The
//! request goes
//! over plain TCP (DNS or an IPv4 literal, then `NetworkHandle::connect`),
//! one request per connection, with a total timeout per attempt
//! ([`DEFAULT_TIMEOUT_MS`], 20 s). The response body is ignored; only the
//! status line is read (see [`classify_status`]). The transport is plain HTTP: TLS can
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

use rohi_connectivity::{AckKind, Disposition, PacketTransport, TransportAck, TransportError};

use crate::endpoint::Endpoint;

const SCHEME: &str = "http";

/// Path of the telemetry ingress.
pub const TELEMETRY_PATH: &str = "/v1/telemetry";
/// `Content-Type` of the request body.
pub const CONTENT_TYPE: &str = "application/protobuf";
/// Default budget of one delivery attempt (DNS, connect, send, response).
pub const DEFAULT_TIMEOUT_MS: u32 = 20_000;

const HEAD_CAPACITY: usize = 256;
/// Longest `User-Agent` value that fits the request head next to the longest
/// endpoint the parser accepts.
pub const MAX_USER_AGENT_LEN: usize = 48;
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
/// use rohi_connectivity::{AckKind, PacketTransport, TransportAck};
/// use rohi_connectivity_http::{Endpoint, HttpTransport, request_head};
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
/// let agent = "urban-esp32c3/0.8.0+5c582f4a.20261122";
/// let head = request_head(endpoint, packet.len(), Some(agent)).unwrap();
/// let mut transport = HttpTransport::new(NetworkHandle::new(&ip), NeverDelay)
///     .with_user_agent(agent);
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
/// let (result, received) = block_on(join(transport.deliver("http://ingress.test", &packet), server));
/// assert_eq!(result, Ok(TransportAck { kind: AckKind::Accepted, detail: 202 }));
/// assert!(received.starts_with(head.as_bytes()));
/// assert!(received.ends_with(&packet)); // the exact signed bytes
/// ```
pub struct HttpTransport<'a, B, D> {
    net: NetworkHandle<'a, B>,
    delay: D,
    timeout_ms: u32,
    user_agent: Option<&'a str>,
}

impl<'a, B: IpBackend, D: DelayNs> HttpTransport<'a, B, D> {
    /// A transport over `net`; `delay` bounds every attempt.
    pub fn new(net: NetworkHandle<'a, B>, delay: D) -> Self {
        Self {
            net,
            delay,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            user_agent: None,
        }
    }

    /// Replaces the budget of one attempt.
    pub fn with_timeout_ms(mut self, timeout_ms: u32) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }

    /// Sends `User-Agent: <user_agent>` with every request. The value is at
    /// most [`MAX_USER_AGENT_LEN`] bytes of printable ASCII; anything else
    /// fails the attempt with [`TransportError::BadResponse`], see
    /// [`request_head`].
    pub fn with_user_agent(mut self, user_agent: &'a str) -> Self {
        self.user_agent = Some(user_agent);
        self
    }

    async fn exchange(
        net: NetworkHandle<'a, B>,
        endpoint: Endpoint<'_>,
        user_agent: Option<&str>,
        packet: &[u8],
    ) -> Result<u16, TransportError> {
        let address = match endpoint.host.parse::<IpAddr>() {
            Ok(address) => address,
            Err(_) => net
                .resolve(endpoint.host, AddrType::IPv4)
                .await
                .map_err(map_error)?,
        };
        log::trace!(
            target: "connectivity.http",
            "Connecting (addr={} port={})",
            address,
            endpoint.port
        );
        let mut connection = net
            .connect(SocketAddr::new(address, endpoint.port))
            .await
            .map_err(map_error)?;

        log::trace!(target: "connectivity.http", "Connected");
        let head = request_head(endpoint, packet.len(), user_agent)?;
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
                log::debug!(target: "connectivity.http", "No status line in response (size_b={})", filled);
                return Err(TransportError::BadResponse);
            }
            match connection.read(&mut response[filled..]).await {
                Ok(0) => {
                    log::debug!(target: "connectivity.http", "Connection closed before the status line");
                    return Err(TransportError::Io);
                }
                Ok(read) => filled += read,
                Err(error) => return Err(map_error(error)),
            }
        }
    }
}

impl<B: IpBackend, D: DelayNs> PacketTransport for HttpTransport<'_, B, D> {
    fn handles(&self, scheme: &str) -> bool {
        scheme == SCHEME
    }

    async fn deliver(
        &mut self,
        endpoint_uri: &str,
        packet: &[u8],
    ) -> Result<TransportAck, TransportError> {
        // Parsed before any I/O; a URI this transport cannot serve is not retried.
        let endpoint = Endpoint::parse(endpoint_uri).ok_or(TransportError::Unsupported)?;
        log::debug!(
            target: "connectivity.http",
            "Posting packet (size_b={} port={} host={})",
            packet.len(),
            endpoint.port,
            endpoint.host
        );
        let timeout_ms = self.timeout_ms;
        let exchange = Self::exchange(self.net, endpoint, self.user_agent, packet);
        let status = with_timeout(&mut self.delay, self.timeout_ms, exchange)
            .await
            .map_err(|_| {
                log::debug!(target: "connectivity.http", "No answer from server (timeout_ms={})", timeout_ms);
                TransportError::Timeout
            })??;
        log::debug!(target: "connectivity.http", "Server answered (status={})", status);
        classify_status(status)
    }
}

/// Maps an HTTP status code to the delivery result: only `202 Accepted`
/// commits the packet.
///
/// # Errors
///
/// [`TransportError::Rejected`] for every status other than `202`; its
/// disposition says what to do with it:
///
/// | Status | Disposition |
/// |--------|-------------|
/// | `408`, `429`, `5xx` | [`Transient`](Disposition::Transient) |
/// | `1xx`, `3xx`, `2xx` other than `202` (captive portal, proxy) | transient (not an acknowledgement) |
/// | `401`, `403` | [`Unauthorized`](Disposition::Unauthorized) |
/// | `400`, `413`, any other `4xx` | [`Permanent`](Disposition::Permanent): the packet is dropped and **not** offered to other endpoints |
///
/// ```
/// use rohi_connectivity::{AckKind, Disposition, TransportAck, TransportError};
/// use rohi_connectivity_http::classify_status;
///
/// assert_eq!(
///     classify_status(202),
///     Ok(TransportAck { kind: AckKind::Accepted, detail: 202 })
/// );
/// // Not an acceptance (a captive portal answers 200): the packet is kept.
/// assert_eq!(classify_status(200).unwrap_err().disposition(), Disposition::Transient);
/// assert_eq!(classify_status(401).unwrap_err().disposition(), Disposition::Unauthorized);
/// assert_eq!(classify_status(413).unwrap_err().disposition(), Disposition::Permanent);
/// ```
pub const fn classify_status(status: u16) -> Result<TransportAck, TransportError> {
    let disposition = match status {
        202 => {
            return Ok(TransportAck {
                kind: AckKind::Accepted,
                detail: 202,
            });
        }
        // Request timeout, throttling and server failures are transient.
        408 | 429 | 500..=599 => Disposition::Transient,
        401 | 403 => Disposition::Unauthorized,
        // Malformed (400), too large (413) and every other client error.
        400..=499 => Disposition::Permanent,
        // Informational, redirects and 2xx other than 202 are not an acceptance.
        _ => Disposition::Transient,
    };
    Err(TransportError::Rejected {
        code: status,
        disposition,
    })
}

#[inline(never)]
fn map_error(error: NetError) -> TransportError {
    log::debug!(target: "connectivity.http", "Network error (err={:?})", error);
    match error {
        NetError::NetworkUnavailable => TransportError::NetworkUnavailable,
        NetError::DnsFailed => TransportError::Dns,
        NetError::ConnectFailed | NetError::NoSocketAvailable => TransportError::Connect,
        NetError::Timeout => TransportError::Timeout,
        NetError::ConnectionReset | NetError::Backend => TransportError::Io,
    }
}

/// The request line and headers for a body of `content_length` bytes, with a
/// `User-Agent` header when `user_agent` is set.
///
/// # Errors
///
/// [`TransportError::BadResponse`] if the head does not fit its 256-byte
/// buffer (a host and port that the endpoint parser would not accept, or a
/// `user_agent` over [`MAX_USER_AGENT_LEN`] bytes) or if `user_agent` has a
/// byte that is not printable ASCII.
///
/// ```
/// use rohi_connectivity_http::{Endpoint, request_head};
///
/// let endpoint = Endpoint::parse("http://ingress.example.org:8080").unwrap();
/// assert_eq!(
///     request_head(endpoint, 194, None).unwrap().as_str(),
///     "POST /v1/telemetry HTTP/1.1\r\n\
///      Host: ingress.example.org:8080\r\n\
///      Content-Type: application/protobuf\r\n\
///      Content-Length: 194\r\n\
///      Connection: close\r\n\r\n"
/// );
/// assert!(
///     request_head(endpoint, 194, Some("urban-std/0.8.0+5c582f4a.20261122"))
///         .unwrap()
///         .contains("User-Agent: urban-std/0.8.0+5c582f4a.20261122\r\n")
/// );
/// ```
pub fn request_head(
    endpoint: Endpoint<'_>,
    content_length: usize,
    user_agent: Option<&str>,
) -> Result<String<HEAD_CAPACITY>, TransportError> {
    if user_agent.is_some_and(|agent| {
        agent.len() > MAX_USER_AGENT_LEN || !agent.bytes().all(|byte| (0x20..0x7f).contains(&byte))
    }) {
        log::warn!(target: "connectivity.http", "User agent rejected (max_b={})", MAX_USER_AGENT_LEN);
        return Err(TransportError::BadResponse);
    }
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
                "Content-Type: {CONTENT_TYPE}\r\nContent-Length: {content_length}\r\n"
            )
        })
        .and_then(|()| match user_agent {
            Some(agent) => write!(head, "User-Agent: {agent}\r\n"),
            None => Ok(()),
        })
        .and_then(|()| write!(head, "Connection: close\r\n\r\n"))
        .map_err(|_| {
            log::warn!(
                target: "connectivity.http",
                "Request head does not fit (capacity_b={})",
                HEAD_CAPACITY
            );
            TransportError::BadResponse
        })?;
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
        log::debug!(
            target: "connectivity.http",
            "Malformed status line (len_b={})",
            line.len()
        );
        return Err(TransportError::BadResponse);
    }
    let digits = &line[9..12];
    if !digits.iter().all(u8::is_ascii_digit) || line.get(12).is_some_and(|b| *b != b' ') {
        log::debug!(target: "connectivity.http", "Malformed status code");
        return Err(TransportError::BadResponse);
    }
    let status = digits
        .iter()
        .fold(0u16, |acc, digit| acc * 10 + u16::from(digit - b'0'));
    Ok(Some(status))
}

#[cfg(test)]
mod tests;
