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

//! Host-compiled example of the IP data plane, included in the SDK book
//! (`docs/sdk/net.md`, `docs/guides/use-network.md`). Keep the anchored
//! regions self-contained: they are shown verbatim.

use core::net::{IpAddr, Ipv4Addr, SocketAddr};

use embassy_futures::block_on;
use embedded_io_async::{Read, Write};
use embedded_nal_async::{AddrType, Dns, TcpConnect};
use rohi_net::testing::FakeIp;
use rohi_net::{NetError, NetworkHandle};

// ANCHOR: fetch
/// Sends `request` to `host:port` and reads the first chunk of the answer.
///
/// Generic over the standard traits: it works with `NetworkHandle`, with any
/// other `embedded-nal-async` stack, and it is what a TLS or HTTP layer would
/// build on.
async fn exchange<N>(
    net: &N,
    host: &str,
    port: u16,
    request: &[u8],
    answer: &mut [u8],
) -> Result<usize, NetError>
where
    N: Dns<Error = NetError> + TcpConnect<Error = NetError>,
{
    let address = net.get_host_by_name(host, AddrType::IPv4).await?;
    let mut connection = net.connect(SocketAddr::new(address, port)).await?;
    connection.write_all(request).await?;
    connection.flush().await?;
    connection.read(answer).await
}
// ANCHOR_END: fetch

#[test]
fn book_example_runs_against_the_fake_stack() {
    let server = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7));
    let remote = SocketAddr::new(server, 8883);

    let ip = FakeIp::<2, 128>::new();
    ip.add_host("uplink.example", server);
    let net = NetworkHandle::new(&ip);

    // Wi-Fi is not connected yet: a typed error, not a DNS or connect failure.
    let mut answer = [0u8; 16];
    assert_eq!(
        block_on(exchange(
            &net,
            "uplink.example",
            8883,
            b"hello",
            &mut answer
        )),
        Err(NetError::NetworkUnavailable)
    );

    ip.set_ready(true);
    let read = block_on(async {
        let mut call = core::pin::pin!(exchange(
            &net,
            "uplink.example",
            8883,
            b"hello",
            &mut answer
        ));
        // Poll once: the connection opens and the request is written.
        assert!(embassy_futures::poll_once(call.as_mut()).is_pending());
        assert_eq!(ip.peer_received(remote).as_slice(), b"hello");
        ip.peer_send(remote, b"world");
        call.await
    });
    assert_eq!(read, Ok(5));
    assert_eq!(&answer[..5], b"world");
    assert_eq!(ip.open_connections(), 0);
}
