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
//! record, a Protobuf message behind a 4-byte little-endian length
//! ([`rohi_control_tcp::frame`], after the plaintext preamble: what a `protected` endpoint serves
//! for reads and an `open` one for everything) or an authenticated and encrypted record of the
//! channel that [`TcpLink::connect_secure`] set up with the pre-shared key
//! ([`rohi_control_tcp::secure`]: every request in every mode, so the way to mutate and
//! provision on a `protected` or `private` endpoint). There is no fragmentation and no BLE framing; the
//! [`Session`](crate::Session) hands the encoded request to the link and gets
//! the encoded response back (see [`Framing::Messages`]). Everything above,
//! [`Device`](crate::Device) and the commands of the CLI, is the same for BLE
//! and TCP.

use std::time::Duration;

use rohi_control_proto::limits::{MAX_REQUEST_LEN, MAX_RESPONSE_LEN};
use rohi_control_tcp::frame::{HEADER_LEN, decode_header, encode_header, plain_preamble};
use rohi_control_tcp::secure::{
    Channel, ClientHandshake, NONCE_LEN, RECORD_HEADER_LEN, SERVER_HELLO_LEN, TAG_LEN,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::error::{Error, Result};
use crate::psk::TcpKey;
use crate::session::{ClientTransport, Framing};

/// A connected TCP device.
pub struct TcpLink {
    stream: TcpStream,
    buffer: Vec<u8>,
    // `Some` after the handshake: the records are sealed with the keys of the handshake.
    channel: Option<Channel>,
}

fn link_error(error: impl std::fmt::Display) -> Error {
    Error::Tcp(error.to_string())
}

impl TcpLink {
    async fn open(endpoint: &str, wait: Duration) -> Result<Self> {
        let stream = timeout(wait, TcpStream::connect(endpoint))
            .await
            .map_err(|_| link_error(format!("{endpoint}: connection timed out")))?
            .map_err(|e| link_error(format!("{endpoint}: {e}")))?;
        stream.set_nodelay(true).map_err(link_error)?;
        Ok(Self {
            stream,
            buffer: Vec::new(),
            channel: None,
        })
    }

    /// Connects to `endpoint` (`host:port`), waiting at most `wait`, with plaintext records:
    /// the plaintext preamble, then length-prefixed records. A `protected` endpoint answers reads
    /// and refuses the rest, an `open` one serves everything but secret actions, and a
    /// `private` one hangs up.
    ///
    /// # Errors
    ///
    /// [`Error::Tcp`] when the endpoint cannot be reached in time.
    pub async fn connect(endpoint: &str, wait: Duration) -> Result<Self> {
        let mut link = Self::open(endpoint, wait).await?;
        link.stream
            .write_all(&plain_preamble())
            .await
            .map_err(link_error)?;
        Ok(link)
    }

    /// Connects to `endpoint` (`host:port`) and authenticates with `key`: the handshake of
    /// [`rohi_control_tcp::secure`], then authenticated and encrypted records only. A device that
    /// does not know the key is refused, never trusted.
    ///
    /// # Errors
    ///
    /// [`Error::Tcp`] when the endpoint cannot be reached in time or closes the connection, and
    /// [`Error::Unauthorized`] when the device does not prove that it knows the key.
    pub async fn connect_secure(endpoint: &str, wait: Duration, key: &TcpKey) -> Result<Self> {
        let mut link = Self::open(endpoint, wait).await?;
        let mut nonce = [0u8; NONCE_LEN];
        crate::psk::random(&mut nonce)?;
        let (handshake, hello) = ClientHandshake::start(key.as_bytes(), &nonce);
        let exchange = async {
            link.stream.write_all(&hello).await?;
            link.stream.flush().await?;
            let mut server_hello = [0u8; SERVER_HELLO_LEN];
            link.stream.read_exact(&mut server_hello).await?;
            Ok::<_, std::io::Error>(server_hello)
        };
        let server_hello = timeout(wait, exchange)
            .await
            .map_err(|_| link_error(format!("{endpoint}: the handshake timed out")))?
            .map_err(|e| {
                link_error(format!(
                    "{endpoint}: the device closed the connection during the handshake ({e}); \
                     does it have a key, and is it this one?"
                ))
            })?;
        let (channel, finish) = handshake.on_server_hello(&server_hello).map_err(|_| {
            Error::Unauthorized("the device did not prove that it knows the key".into())
        })?;
        link.stream.write_all(&finish).await.map_err(link_error)?;
        link.stream.flush().await.map_err(link_error)?;
        link.channel = Some(channel);
        Ok(link)
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
        if self.channel.is_some() {
            return self.take_sealed_record();
        }
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

impl TcpLink {
    // A whole sealed record of the buffer, opened and removed from it. Nothing is consumed (and no
    // counter moves) before the record is complete, so a cancelled `recv` loses nothing.
    fn take_sealed_record(&mut self) -> Result<Option<Vec<u8>>> {
        let Some(channel) = self.channel.as_mut() else {
            return Ok(None);
        };
        let Some(header) = self.buffer.first_chunk::<RECORD_HEADER_LEN>() else {
            return Ok(None);
        };
        let header = *header;
        let length = channel
            .open_header(header, MAX_RESPONSE_LEN)
            .map_err(|e| Error::Protocol(format!("invalid record from the device: {e:?}")))?;
        let total = RECORD_HEADER_LEN + length + TAG_LEN;
        if self.buffer.len() < total {
            return Ok(None);
        }
        let mut data = self.buffer[RECORD_HEADER_LEN..RECORD_HEADER_LEN + length].to_vec();
        let mut tag = [0u8; TAG_LEN];
        tag.copy_from_slice(&self.buffer[RECORD_HEADER_LEN + length..total]);
        self.buffer.drain(..total);
        channel.open(header, &mut data, &tag).map_err(|e| {
            Error::Protocol(format!(
                "a record of the device did not authenticate: {e:?}"
            ))
        })?;
        Ok(Some(data))
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
        let mut record = Vec::with_capacity(HEADER_LEN + message.len() + TAG_LEN);
        if let Some(channel) = self.channel.as_mut() {
            let mut data = message.to_vec();
            let (header, tag) = channel
                .seal(&mut data)
                .map_err(|e| Error::Protocol(format!("cannot seal the request: {e:?}")))?;
            record.extend_from_slice(&header);
            record.extend_from_slice(&data);
            record.extend_from_slice(&tag);
        } else {
            record.extend_from_slice(&encode_header(message.len() as u32));
            record.extend_from_slice(message);
        }
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
            let mut preamble = [0u8; 5];
            stream.read_exact(&mut preamble).await.unwrap();
            assert_eq!(preamble, plain_preamble());
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

#[cfg(test)]
mod secure_tests {
    use rohi_control_tcp::secure::{CLIENT_FINISH_LEN, CLIENT_HELLO_LEN, ServerHandshake};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    const KEY: [u8; 32] = [0x33; 32];

    // A device that authenticates with `KEY` and echoes every record back, sealed.
    async fn echo_device(key: [u8; 32], records: usize) -> (String, tokio::task::JoinHandle<bool>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut hello = [0u8; CLIENT_HELLO_LEN];
            stream.read_exact(&mut hello).await.unwrap();
            let (handshake, server_hello) =
                ServerHandshake::respond(&key, &hello, &[0x44; 32]).unwrap();
            stream.write_all(&server_hello).await.unwrap();
            let mut finish = [0u8; CLIENT_FINISH_LEN];
            if stream.read_exact(&mut finish).await.is_err() {
                return false;
            }
            let Ok(mut channel) = handshake.finish(&finish) else {
                return false;
            };
            for _ in 0..records {
                let mut header = [0u8; RECORD_HEADER_LEN];
                stream.read_exact(&mut header).await.unwrap();
                let len = channel.open_header(header, 2048).unwrap();
                let mut data = vec![0u8; len];
                stream.read_exact(&mut data).await.unwrap();
                let mut tag = [0u8; TAG_LEN];
                stream.read_exact(&mut tag).await.unwrap();
                channel.open(header, &mut data, &tag).unwrap();
                let (header, tag) = channel.seal(&mut data).unwrap();
                stream.write_all(&header).await.unwrap();
                stream.write_all(&data).await.unwrap();
                stream.write_all(&tag).await.unwrap();
            }
            true
        });
        (address, task)
    }

    #[tokio::test]
    async fn requests_and_responses_are_sealed_records() {
        let (address, device) = echo_device(KEY, 2).await;
        let key = TcpKey::from_bytes(KEY).unwrap();
        let mut link = TcpLink::connect_secure(&address, Duration::from_secs(5), &key)
            .await
            .unwrap();
        for message in [b"first request".as_slice(), b"second"] {
            link.send(message).await.unwrap();
            assert_eq!(link.recv().await.unwrap(), message);
        }
        assert!(device.await.unwrap());
    }

    #[tokio::test]
    async fn a_device_that_does_not_know_the_key_is_refused() {
        let (address, device) = echo_device([0x55; 32], 0).await;
        let key = TcpKey::from_bytes(KEY).unwrap();
        let result = TcpLink::connect_secure(&address, Duration::from_secs(5), &key).await;
        assert!(matches!(result, Err(Error::Unauthorized(_))));
        // It was never sent the final message, so it never authenticated the client either.
        assert!(!device.await.unwrap());
    }

    #[tokio::test]
    async fn a_device_that_closes_during_the_handshake_is_reported_as_such() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            // An endpoint without a key hangs up on a hello.
            let (stream, _) = listener.accept().await.unwrap();
            drop(stream);
        });
        let key = TcpKey::from_bytes(KEY).unwrap();
        let result = TcpLink::connect_secure(&address, Duration::from_secs(5), &key).await;
        let Err(Error::Tcp(message)) = result else {
            panic!("expected a TCP error")
        };
        assert!(message.contains("handshake"), "{message}");
    }

    #[tokio::test]
    async fn a_tampered_response_is_a_protocol_error_and_not_data() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut hello = [0u8; CLIENT_HELLO_LEN];
            stream.read_exact(&mut hello).await.unwrap();
            let (handshake, server_hello) =
                ServerHandshake::respond(&KEY, &hello, &[0x44; 32]).unwrap();
            stream.write_all(&server_hello).await.unwrap();
            let mut finish = [0u8; CLIENT_FINISH_LEN];
            stream.read_exact(&mut finish).await.unwrap();
            let mut channel = handshake.finish(&finish).unwrap();
            let mut data = *b"a response";
            let (header, tag) = channel.seal(&mut data).unwrap();
            data[0] ^= 1;
            stream.write_all(&header).await.unwrap();
            stream.write_all(&data).await.unwrap();
            stream.write_all(&tag).await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let key = TcpKey::from_bytes(KEY).unwrap();
        let mut link = TcpLink::connect_secure(&address, Duration::from_secs(5), &key)
            .await
            .unwrap();
        assert!(matches!(link.recv().await, Err(Error::Protocol(_))));
    }
}
