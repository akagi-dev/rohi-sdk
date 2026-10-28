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
//! Host: <host>
//! Content-Type: application/protobuf
//! Content-Length: <frame length>
//! Connection: close
//!
//! <exact SignedEnvelope bytes>
//! ```
//!
//! One request per connection. The response body is ignored; only the status
//! line is read (see [`classify_status`]). The transport is plain HTTP: TLS
//! can later wrap the connection without touching the message or signature.

use core::fmt::Write as _;
use core::net::{IpAddr, SocketAddr};

use embedded_hal_async::delay::DelayNs;
use embedded_io_async::{Read, Write};
use embedded_nal_async::AddrType;
use heapless::String;
use rohi_hal::runtime::with_timeout;
use rohi_net::{IpBackend, NetError, NetworkHandle};

use crate::config::Endpoint;
use crate::envelope::EnvelopeFrame;
use crate::transport::{EnvelopeTransport, TransportAck, TransportError, classify_status};

/// Path of the telemetry ingress.
pub const TELEMETRY_PATH: &str = "/v1/telemetry";
/// `Content-Type` of the request body.
pub const CONTENT_TYPE: &str = "application/protobuf";
/// Default budget of one delivery attempt (DNS, connect, send, response).
pub const DEFAULT_TIMEOUT_MS: u32 = 20_000;

const HEAD_CAPACITY: usize = 256;
const RESPONSE_CAPACITY: usize = 64;

/// HTTP transport of [`EnvelopeFrame`]s.
pub struct HttpTransport<'a, B, D> {
    net: NetworkHandle<'a, B>,
    endpoint: Endpoint<'a>,
    delay: D,
    timeout_ms: u32,
}

impl<'a, B: IpBackend, D: DelayNs> HttpTransport<'a, B, D> {
    /// A transport posting to `endpoint` through `net`, with the default
    /// timeout. `delay` bounds every attempt.
    pub fn new(net: NetworkHandle<'a, B>, endpoint: Endpoint<'a>, delay: D) -> Self {
        Self {
            net,
            endpoint,
            delay,
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }

    /// Replaces the budget of one delivery attempt.
    pub fn with_timeout_ms(mut self, timeout_ms: u32) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }

    async fn exchange(
        net: NetworkHandle<'a, B>,
        endpoint: Endpoint<'a>,
        frame: &EnvelopeFrame,
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

        let head = request_head(endpoint, frame.len())?;
        connection
            .write_all(head.as_bytes())
            .await
            .map_err(map_error)?;
        connection
            .write_all(frame.as_bytes())
            .await
            .map_err(map_error)?;
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

impl<B: IpBackend, D: DelayNs> EnvelopeTransport for HttpTransport<'_, B, D> {
    async fn send(&mut self, frame: &EnvelopeFrame) -> Result<TransportAck, TransportError> {
        let exchange = Self::exchange(self.net, self.endpoint, frame);
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
