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

//! The TCP side of the control transport: a listener that yields clients to
//! [`ControlTcp`](rohi_control_tcp::ControlTcp).

use core::net::SocketAddr;
use std::io::ErrorKind;

use embedded_io_async::{ErrorType, Read, Write};
use log::warn;
use rohi_control_tcp::Acceptor;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

use crate::runtime::{handle, within};

/// A listening TCP socket, the [`Acceptor`] of the control transport.
///
/// The socket is bound by the caller, who also decides the address. For a
/// development transport that is the loopback address: nothing here makes a
/// non-loopback listener safe, see [`rohi_control_tcp`].
pub struct TcpAcceptor {
    listener: TcpListener,
}

impl TcpAcceptor {
    /// Takes over a listener that was bound by the caller.
    ///
    /// # Errors
    ///
    /// The socket cannot be switched to non-blocking mode or registered with
    /// the reactor.
    pub fn new(listener: std::net::TcpListener) -> std::io::Result<Self> {
        listener.set_nonblocking(true)?;
        let _entered = handle().enter();
        Ok(Self {
            listener: TcpListener::from_std(listener)?,
        })
    }

    /// Binds a listener to `address` (port 0 picks a free port).
    ///
    /// # Errors
    ///
    /// The address cannot be bound.
    pub fn bind(address: SocketAddr) -> std::io::Result<Self> {
        Self::new(std::net::TcpListener::bind(address)?)
    }

    /// The address the listener is bound to, with the port that was chosen.
    ///
    /// # Errors
    ///
    /// The operating system cannot tell it.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }
}

impl Acceptor for TcpAcceptor {
    type Stream = TcpConnection;
    type Error = std::io::Error;

    /// Waits for the next client. A client that gives up before it is accepted
    /// is skipped; any other failure of the listener is returned.
    async fn accept(&mut self) -> Result<TcpConnection, std::io::Error> {
        loop {
            match within(self.listener.accept()).await {
                Ok((stream, peer)) => {
                    let _ = stream.set_nodelay(true);
                    log::debug!("control: connection from {peer}");
                    return Ok(TcpConnection { stream });
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::ConnectionAborted
                            | ErrorKind::ConnectionReset
                            | ErrorKind::Interrupted
                            | ErrorKind::TimedOut
                    ) =>
                {
                    warn!("control: accept skipped: {error}");
                }
                Err(error) => return Err(error),
            }
        }
    }
}

/// A client connection of a [`TcpAcceptor`]. Dropping it closes the socket.
pub struct TcpConnection {
    stream: TcpStream,
}

impl ErrorType for TcpConnection {
    type Error = std::io::Error;
}

impl Read for TcpConnection {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        within(self.stream.read(buf)).await
    }
}

impl Write for TcpConnection {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, std::io::Error> {
        within(self.stream.write(buf)).await
    }

    async fn flush(&mut self) -> Result<(), std::io::Error> {
        within(self.stream.flush()).await
    }
}
