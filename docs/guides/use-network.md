# Use IP networking

Open DNS and TCP connections through `NetworkHandle`. Never use `embassy-net` or any vendor type directly: code written
against the standard traits is portable, testable on the host and independent of the stack. Background:
[Networking](../sdk/net.md).

## Write portable code

Make your layer generic over `embedded_nal_async::{Dns, TcpConnect}` and `embedded_io_async::{Read, Write}`:

```rust,ignore
{{#include ../../crates/rohi-net/tests/network_example.rs:fetch}}
```

Handle the typed `NetError`:

- `NetworkUnavailable`: Wi-Fi is not connected (or DHCP incomplete). Wait for `WifiStatus` `Connected` (see
  `WifiHandle::status_receiver`) and retry; this is not a server problem.
- `DnsFailed` / `ConnectFailed`: the network is up but the name or endpoint failed; apply your own retry policy.
- `NoSocketAvailable`: all TCP slots of the product are in use; the call does **not** wait. Close a connection first.
- `ConnectionReset` / `Timeout`: the connection is gone; reconnect.

Always drop connections you no longer need: the slot is released on drop. Bound waits with your own timeout
(`embassy_time::with_timeout`); dropping a pending `connect` is safe and frees its slot
([cancellation](../concepts/async-and-cancellation.md)).

## Test on the host

```rust,ignore
let ip = rohi_net::testing::FakeIp::<2, 128>::new();   // feature `testing`
ip.add_host("uplink.example", server);
ip.set_ready(true);
let net = NetworkHandle::new(&ip);
```

`FakeIp` scripts DNS answers, refused connections, readiness changes, slot exhaustion and held operations and lets you
play the peer. See `crates/rohi-net/src/ip/tests.rs` and `tests/network_example.rs`.

## On the device

The firmware creates the handle when it starts Wi-Fi (`altruist_urban::net::start` returns
`(EspWifi, NetRunner, NetworkHandle<'static, Ip>)`) and hands it to the component that needs it. Capacity (`TCP_CONNECTIONS`,
buffer sizes) is a product decision in `firmwares/altruist-urban/src/resources.rs`
([Resource limits](../reference/resource-limits.md#ip-data-plane)). Remember there is no TLS yet: do not send secrets
in clear text.
