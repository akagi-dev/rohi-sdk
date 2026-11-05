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

//! TCP link to a device that serves the control protocol over a socket, such as
//! the `std` Altruist Urban (`altruist-urban`).
//!
//! The link carries whole messages: every request and every response is one
//! record of [`rohi_control_tcp::frame`], a 4-byte little-endian length and a
//! Protobuf message. There is no fragmentation and no BLE framing; the
//! [`Session`](crate::Session) hands the encoded request to the link and gets
//! the encoded response back (see [`Framing::Messages`]). Everything above,
//! [`Device`](crate::Device) and the commands of the CLI, is the same for BLE
//! and TCP.

use std::time::Duration;

use rohi_control_proto::limits::{MAX_REQUEST_LEN, MAX_RESPONSE_LEN};
use rohi_control_tcp::frame::{HEADER_LEN, decode_header, encode_header};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::error::{Error, Result};
use crate::session::{ClientTransport, Framing};

/// A connected TCP device.
pub struct TcpLink {
    stream: TcpStream,
    buffer: Vec<u8>,
}

fn link_error(error: impl std::fmt::Display) -> Error {
    Error::Tcp(error.to_string())
}

impl TcpLink {
    /// Connects to `endpoint` (`host:port`), waiting at most `wait`.
    ///
    /// # Errors
    ///
    /// [`Error::Tcp`] when the endpoint cannot be reached in time.
    pub async fn connect(endpoint: &str, wait: Duration) -> Result<Self> {
        let stream = timeout(wait, TcpStream::connect(endpoint))
            .await
            .map_err(|_| link_error(format!("{endpoint}: connection timed out")))?
            .map_err(|e| link_error(format!("{endpoint}: {e}")))?;
        stream.set_nodelay(true).map_err(link_error)?;
        Ok(Self {
            stream,
            buffer: Vec::new(),
        })
    }

    /// Closes the connection.
    pub async fn disconnect(mut self) -> Result<()> {
        self.stream.shutdown().await.map_err(link_error)
    }

    /// The first complete record of the buffer, removed from it.
    ///
    /// A length that no response can have is an error: the stream cannot be
    /// resynchronized, so it is not read on.
    fn take_record(&mut self) -> Result<Option<Vec<u8>>> {
        let Some(header) = self.buffer.first_chunk::<HEADER_LEN>() else {
            return Ok(None);
        };
        let length = decode_header(*header, MAX_RESPONSE_LEN)
            .map_err(|e| Error::Protocol(format!("invalid record from the device: {e}")))?;
        if self.buffer.len() < HEADER_LEN + length {
            return Ok(None);
        }
        let record = self.buffer[HEADER_LEN..HEADER_LEN + length].to_vec();
        self.buffer.drain(..HEADER_LEN + length);
        Ok(Some(record))
    }
}

impl ClientTransport for TcpLink {
    fn max_payload(&self) -> usize {
        MAX_REQUEST_LEN
    }

    fn framing(&self) -> Framing {
        Framing::Messages
    }

    async fn send(&mut self, message: &[u8]) -> Result<()> {
        if message.is_empty() || message.len() > MAX_REQUEST_LEN {
            return Err(Error::TooLarge);
        }
        let mut record = Vec::with_capacity(HEADER_LEN + message.len());
        record.extend_from_slice(&encode_header(message.len() as u32));
        record.extend_from_slice(message);
        self.stream.write_all(&record).await.map_err(link_error)?;
        self.stream.flush().await.map_err(link_error)
    }

    /// Cancel safe: bytes already read stay in the link for the next call.
    async fn recv(&mut self) -> Result<Vec<u8>> {
        loop {
            if let Some(record) = self.take_record()? {
                return Ok(record);
            }
            let mut chunk = [0u8; 512];
            let read = self.stream.read(&mut chunk).await.map_err(link_error)?;
            if read == 0 {
                return Err(link_error("connection closed"));
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    #[tokio::test]
    async fn messages_travel_as_length_prefixed_records() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut head = [0u8; HEADER_LEN];
            stream.read_exact(&mut head).await.unwrap();
            let mut message = vec![0u8; u32::from_le_bytes(head) as usize];
            stream.read_exact(&mut message).await.unwrap();
            // Answer with two records, the first one split across writes.
            let mut records = Vec::new();
            for payload in [message.as_slice(), b"second".as_slice()] {
                records.extend_from_slice(&encode_header(payload.len() as u32));
                records.extend_from_slice(payload);
            }
            let (first, rest) = records.split_at(3);
            stream.write_all(first).await.unwrap();
            stream.flush().await.unwrap();
            tokio::task::yield_now().await;
            stream.write_all(rest).await.unwrap();
        });

        let mut link = TcpLink::connect(&address.to_string(), Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(link.framing(), Framing::Messages);
        assert_eq!(link.max_payload(), MAX_REQUEST_LEN);
        link.send(b"request").await.unwrap();
        assert_eq!(link.recv().await.unwrap(), b"request");
        assert_eq!(link.recv().await.unwrap(), b"second");
        server.await.unwrap();
        assert!(matches!(link.recv().await, Err(Error::Tcp(_))));
    }

    #[tokio::test]
    async fn an_oversized_record_from_the_device_is_a_protocol_error() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream
                .write_all(&encode_header(MAX_RESPONSE_LEN as u32 + 1))
                .await
                .unwrap();
            stream.flush().await.unwrap();
            // Keep the connection open: the client must give up by itself.
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let mut link = TcpLink::connect(&address.to_string(), Duration::from_secs(5))
            .await
            .unwrap();
        assert!(matches!(link.recv().await, Err(Error::Protocol(_))));
        server.abort();
    }

    #[tokio::test]
    async fn an_empty_or_oversized_request_is_not_sent() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut link = TcpLink::connect(&address.to_string(), Duration::from_secs(5))
            .await
            .unwrap();
        assert!(matches!(link.send(&[]).await, Err(Error::TooLarge)));
        assert!(matches!(
            link.send(&vec![0u8; MAX_REQUEST_LEN + 1]).await,
            Err(Error::TooLarge)
        ));
    }

    #[tokio::test]
    async fn an_unreachable_endpoint_is_a_tcp_error() {
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = free.local_addr().unwrap();
        drop(free);
        let result = TcpLink::connect(&address.to_string(), Duration::from_secs(5)).await;
        assert!(matches!(result, Err(Error::Tcp(_))));
    }
}
