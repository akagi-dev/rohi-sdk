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

//! TCP link to a device that speaks the control protocol over a socket, such
//! as the `altruist-sim` simulator.
//!
//! The carrier moves exactly the chunks of the BLE link; only the transport
//! differs. Each chunk (one ATT write or notification on BLE) is one record:
//!
//! ```text
//! +-----------+----------------------------+
//! | len: u16  | one BLE frame, len bytes   |
//! |    LE     | (see rohi_control_ble::frame) |
//! +-----------+----------------------------+
//! ```
//!
//! Everything above, [`Session`](crate::Session), [`Device`](crate::Device) and
//! the commands of the CLI, is the same for BLE and TCP.

use std::time::Duration;

use rohi_net::MAX_BLE_PAYLOAD;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::error::{Error, Result};
use crate::session::ClientTransport;

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

    fn take_record(&mut self) -> Option<Vec<u8>> {
        let length = usize::from(u16::from_le_bytes([
            *self.buffer.first()?,
            *self.buffer.get(1)?,
        ]));
        (self.buffer.len() >= 2 + length).then(|| {
            let record = self.buffer[2..2 + length].to_vec();
            self.buffer.drain(..2 + length);
            record
        })
    }
}

impl ClientTransport for TcpLink {
    fn max_payload(&self) -> usize {
        MAX_BLE_PAYLOAD
    }

    async fn send(&mut self, chunk: &[u8]) -> Result<()> {
        let length = u16::try_from(chunk.len()).map_err(|_| Error::TooLarge)?;
        let mut record = Vec::with_capacity(2 + chunk.len());
        record.extend_from_slice(&length.to_le_bytes());
        record.extend_from_slice(chunk);
        self.stream.write_all(&record).await.map_err(link_error)?;
        self.stream.flush().await.map_err(link_error)
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        loop {
            if let Some(record) = self.take_record() {
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
    async fn chunks_travel_as_length_prefixed_records() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut head = [0u8; 2];
            stream.read_exact(&mut head).await.unwrap();
            let mut chunk = vec![0u8; usize::from(u16::from_le_bytes(head))];
            stream.read_exact(&mut chunk).await.unwrap();
            // Answer with two records, the first one split across writes.
            let mut records = Vec::new();
            for payload in [chunk.as_slice(), b"second".as_slice()] {
                records.extend_from_slice(&(payload.len() as u16).to_le_bytes());
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
        assert_eq!(link.max_payload(), MAX_BLE_PAYLOAD);
        link.send(b"request").await.unwrap();
        assert_eq!(link.recv().await.unwrap(), b"request");
        assert_eq!(link.recv().await.unwrap(), b"second");
        server.await.unwrap();
        assert!(matches!(link.recv().await, Err(Error::Tcp(_))));
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
