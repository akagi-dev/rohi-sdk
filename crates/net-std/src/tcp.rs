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
use core::time::Duration;
use std::io::ErrorKind;
use std::time::Instant;

use embedded_io_async::{ErrorType, Read, Write};
use log::{debug, info, trace, warn};
use rohi_control_tcp::{Acceptor, Timed};
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
        listener.set_nonblocking(true).inspect_err(|error| {
            warn!(target: "control.tcp", "Cannot make the listener non-blocking (err={error})");
        })?;
        let _entered = handle().enter();
        let listener = TcpListener::from_std(listener).inspect_err(|error| {
            warn!(target: "control.tcp", "Cannot register the listener (err={error})");
        })?;
        match listener.local_addr() {
            Ok(address) => info!(target: "control.tcp", "Listening (addr={address})"),
            Err(_) => info!(target: "control.tcp", "Listener started"),
        }
        Ok(Self { listener })
    }

    /// Binds a listener to `address` (port 0 picks a free port).
    ///
    /// # Errors
    ///
    /// The address cannot be bound.
    pub fn bind(address: SocketAddr) -> std::io::Result<Self> {
        let listener = std::net::TcpListener::bind(address).inspect_err(|error| {
            warn!(target: "control.tcp", "Cannot bind the listener (addr={address} err={error})");
        })?;
        Self::new(listener)
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
                    debug!(target: "control.tcp", "Connection accepted (peer={peer})");
                    return Ok(TcpConnection {
                        stream,
                        peer,
                        deadline: None,
                    });
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
                    warn!(target: "control.tcp", "Accept skipped (err={error})");
                }
                Err(error) => {
                    warn!(target: "control.tcp", "Accept failed (err={error})");
                    return Err(error);
                }
            }
        }
    }
}

/// How long a read waits for the client. The control endpoint serves one client at a
/// time, so a client that goes silent (or whose key was replaced) must not hold it forever.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// A client connection of a [`TcpAcceptor`]. Dropping it closes the socket. A read that
/// gets no byte for 60 seconds fails with [`ErrorKind::TimedOut`], and so does any read or write
/// after the absolute deadline of [`Timed::set_deadline`].
pub struct TcpConnection {
    stream: TcpStream,
    peer: SocketAddr,
    deadline: Option<Instant>,
}

impl TcpConnection {
    // The time a read or write may still wait, `None` when it may not wait at all.
    fn remaining(&self, idle: Option<Duration>) -> Option<Duration> {
        let left = self
            .deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()));
        match (left, idle) {
            (Some(left), Some(idle)) => Some(left.min(idle)),
            (Some(left), None) => Some(left),
            (None, idle) => idle,
        }
    }
}

impl Timed for TcpConnection {
    fn set_deadline(&mut self, limit: Option<Duration>) {
        self.deadline = limit.and_then(|limit| Instant::now().checked_add(limit));
    }
}

async fn bounded<T>(
    wait: Option<Duration>,
    operation: impl Future<Output = std::io::Result<T>>,
) -> std::io::Result<T> {
    match wait {
        None => operation.await,
        Some(wait) => tokio::time::timeout(wait, operation)
            .await
            .unwrap_or_else(|_| Err(ErrorKind::TimedOut.into())),
    }
}

impl Drop for TcpConnection {
    fn drop(&mut self) {
        debug!(target: "control.tcp", "Connection closed (peer={})", self.peer);
    }
}

impl ErrorType for TcpConnection {
    type Error = std::io::Error;
}

impl Read for TcpConnection {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        let wait = self.remaining(Some(IDLE_TIMEOUT));
        let read = within(bounded(wait, self.stream.read(buf)))
            .await
            .inspect_err(
            |error| debug!(target: "control.tcp", "Read failed (peer={} err={error})", self.peer),
        )?;
        trace!(target: "control.tcp", "Read from peer (size_b={read} peer={})", self.peer);
        Ok(read)
    }
}

impl Write for TcpConnection {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, std::io::Error> {
        let wait = self.remaining(None);
        let written = within(bounded(wait, self.stream.write(buf)))
            .await
            .inspect_err(
                |error| debug!(target: "control.tcp", "Write failed (peer={} err={error})", self.peer),
            )?;
        trace!(target: "control.tcp", "Wrote to peer (size_b={written} peer={})", self.peer);
        Ok(written)
    }

    async fn flush(&mut self) -> Result<(), std::io::Error> {
        let wait = self.remaining(None);
        within(bounded(wait, self.stream.flush()))
            .await
            .inspect_err(
                |error| debug!(target: "control.tcp", "Flush failed (peer={} err={error})", self.peer),
            )
    }
}
