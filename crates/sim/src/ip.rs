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

//! Faults above the socket layer of the IP data plane.
//!
//! [`SimIp`] wraps a real [`IpBackend`] (in practice `rohi_net_std::NativeIp`,
//! the sockets of the host) and lets the [`World`] decide whether the plane is
//! usable: the simulated Wi-Fi link, offline, DNS and connect failures, resets,
//! latency and dropped connections. The wrapped backend is not changed and does
//! not know about the simulation; the device logic sees only the portable
//! contract.
//!
//! ```text
//! NetworkHandle -> SimIp (faults of the World) -> NativeIp -> host sockets
//! ```

use core::net::{IpAddr, SocketAddr};

use embassy_time::Timer;
use embedded_nal_async::AddrType;
use log::{debug, trace};
use rohi_net::{IpBackend, IpUsage, NetError, UdpSocket};

use crate::command::IpFault;
use crate::world::{World, world};

/// The IP data plane of the host with the faults of the simulated world.
pub struct SimIp<I> {
    inner: I,
    world: &'static World,
}

impl<I: IpBackend> SimIp<I> {
    /// Wraps `inner` with the faults of the [`world`], which must exist.
    pub fn new(inner: I) -> Self {
        Self {
            inner,
            world: world(),
        }
    }

    fn fault(&self) -> Option<IpFault> {
        self.world.lock().ip.fault
    }

    async fn latency(&self) {
        let ms = self.world.lock().ip.latency_ms;
        if ms > 0 {
            Timer::after_millis(u64::from(ms)).await;
        }
    }
}

/// A TCP connection of the wrapped backend, reset by the faults of the world.
pub struct SimConnection<C> {
    inner: C,
    world: &'static World,
}

impl<C> SimConnection<C> {
    fn check(&self) -> Result<(), NetError> {
        let state = self.world.lock();
        if !state.wifi.link_up {
            trace!(target: "sim.ip", "Connection reset: the Wi-Fi link is down");
            return Err(NetError::ConnectionReset);
        }
        match state.ip.fault {
            Some(IpFault::Offline) => {
                trace!(target: "sim.ip", "Connection refused: injected offline fault");
                Err(NetError::NetworkUnavailable)
            }
            Some(IpFault::Reset) => {
                trace!(target: "sim.ip", "Connection reset: injected reset fault");
                Err(NetError::ConnectionReset)
            }
            _ => Ok(()),
        }
    }
}

impl<C: embedded_io_async::ErrorType<Error = NetError>> embedded_io_async::ErrorType
    for SimConnection<C>
{
    type Error = NetError;
}

impl<C: embedded_io_async::Read<Error = NetError>> embedded_io_async::Read for SimConnection<C> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, NetError> {
        self.check()?;
        self.inner.read(buf).await
    }
}

impl<C: embedded_io_async::Write<Error = NetError>> embedded_io_async::Write for SimConnection<C> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, NetError> {
        self.check()?;
        self.inner.write(buf).await
    }

    async fn flush(&mut self) -> Result<(), NetError> {
        self.inner.flush().await
    }
}

/// A UDP socket of the wrapped backend, stopped by the faults of the world.
pub struct SimUdp<U> {
    inner: U,
    world: &'static World,
}

impl<U> SimUdp<U> {
    fn check(&self) -> Result<(), NetError> {
        let state = self.world.lock();
        if !state.wifi.link_up || state.ip.fault == Some(IpFault::Offline) {
            trace!(target: "sim.ip", "UDP unavailable (link_up={})", state.wifi.link_up);
            return Err(NetError::NetworkUnavailable);
        }
        Ok(())
    }
}

impl<U: UdpSocket> UdpSocket for SimUdp<U> {
    async fn send_to(&self, data: &[u8], remote: SocketAddr) -> Result<(), NetError> {
        self.check()?;
        self.inner.send_to(data, remote).await
    }

    async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr), NetError> {
        self.check()?;
        self.inner.recv_from(buf).await
    }
}

impl<I: IpBackend> IpBackend for SimIp<I> {
    type Connection<'a>
        = SimConnection<I::Connection<'a>>
    where
        Self: 'a;
    type Udp<'a>
        = SimUdp<I::Udp<'a>>
    where
        Self: 'a;

    /// Ready while the simulated Wi-Fi link is up, no offline fault is set and
    /// the wrapped backend is ready.
    fn is_ready(&self) -> bool {
        let state = self.world.lock();
        state.wifi.link_up && state.ip.fault != Some(IpFault::Offline) && self.inner.is_ready()
    }

    fn usage(&self) -> IpUsage {
        self.inner.usage()
    }

    async fn resolve(&self, host: &str, addr_type: AddrType) -> Result<IpAddr, NetError> {
        self.latency().await;
        if let Some(fault @ (IpFault::Dns | IpFault::Offline)) = self.fault() {
            debug!(target: "sim.ip", "Name resolution failed: injected fault (fault={fault:?} host={host})");
            return Err(NetError::DnsFailed);
        }
        self.inner.resolve(host, addr_type).await.inspect_err(|e| {
            debug!(target: "sim.ip", "Name resolution failed (err={e:?} host={host})");
        })
    }

    async fn connect(&self, remote: SocketAddr) -> Result<Self::Connection<'_>, NetError> {
        self.latency().await;
        {
            let mut state = self.world.lock();
            if !state.wifi.link_up {
                debug!(target: "sim.ip", "Connect failed: the Wi-Fi link is down (remote={remote})");
                return Err(NetError::NetworkUnavailable);
            }
            if let Some(fault @ (IpFault::Connect | IpFault::Offline)) = state.ip.fault {
                debug!(target: "sim.ip", "Connect failed: injected fault (fault={fault:?} remote={remote})");
                return Err(NetError::ConnectFailed);
            }
            if state.ip.drop_next > 0 {
                state.ip.drop_next -= 1;
                debug!(
                    target: "sim.ip",
                    "Connect dropped (remaining={} remote={remote})",
                    state.ip.drop_next
                );
                return Err(NetError::ConnectFailed);
            }
        }
        let inner = self.inner.connect(remote).await.inspect_err(|e| {
            debug!(target: "sim.ip", "Connect failed (err={e:?} remote={remote})");
        })?;
        trace!(target: "sim.ip", "Connected (remote={remote})");
        Ok(SimConnection {
            inner,
            world: self.world,
        })
    }

    fn udp_socket(&self) -> Result<Self::Udp<'_>, NetError> {
        Ok(SimUdp {
            inner: self.inner.udp_socket()?,
            world: self.world,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use embassy_futures::block_on;
    use embedded_io_async::{Read as _, Write as _};
    use rohi_net::NetworkHandle;
    use rohi_net_std::NativeIp;

    use super::*;
    use crate::command::Command;
    use crate::scenario::Scenario;
    use crate::world::init_for_tests;

    /// One test: the world is a process-wide singleton.
    #[test]
    fn the_world_decides_what_the_host_sockets_may_do() {
        init_for_tests(&Scenario::default());
        let ip = SimIp::new(NativeIp::new());
        let net = NetworkHandle::new(&ip);

        // Not ready until the Wi-Fi link is up.
        assert!(!net.is_ready());
        world().lock().wifi.link_up = true;
        assert!(net.is_ready());

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buffer = [0u8; 5];
                if stream.read_exact(&mut buffer).is_ok() {
                    stream.write_all(&buffer).unwrap();
                }
            }
        });

        block_on(async {
            let literal = net.resolve("127.0.0.1", AddrType::IPv4).await.unwrap();
            let remote = SocketAddr::new(literal, port);
            let mut connection = net.connect(remote).await.unwrap();
            connection.write_all(b"hello").await.unwrap();
            let mut echoed = [0u8; 5];
            connection.read_exact(&mut echoed).await.unwrap();
            assert_eq!(&echoed, b"hello");
            assert_eq!(net.usage().tcp_in_use, 1);

            // The bounded pool is the one of the wrapped backend.
            let second = net.connect(remote).await.unwrap();
            assert_eq!(
                net.connect(remote).await.err(),
                Some(NetError::NoSocketAvailable)
            );
            drop(second);

            // Faults above the socket layer.
            world().apply(&Command::IpFail(IpFault::Dns));
            assert_eq!(
                net.resolve("localhost", AddrType::Either).await,
                Err(NetError::DnsFailed)
            );
            world().apply(&Command::IpFail(IpFault::Connect));
            assert_eq!(
                net.connect(remote).await.err(),
                Some(NetError::ConnectFailed)
            );
            world().apply(&Command::IpFail(IpFault::Reset));
            let mut byte = [0u8; 1];
            assert_eq!(
                connection.read(&mut byte).await,
                Err(NetError::ConnectionReset)
            );
            world().apply(&Command::IpRecover);

            // The link going down resets what was open and stops new traffic.
            world().lock().wifi.link_up = false;
            assert_eq!(
                net.connect(remote).await.err(),
                Some(NetError::NetworkUnavailable)
            );
            assert_eq!(
                connection.read(&mut byte).await,
                Err(NetError::ConnectionReset)
            );
        });
        server.join().unwrap();
    }
}
