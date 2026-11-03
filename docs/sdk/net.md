# Networking (`rohi-net`)

`rohi-net` is the portable networking layer of the SDK. It answers three separate questions for firmware: which Wi-Fi
network should the radio use and is it connected, can I resolve a name or open a TCP connection or UDP socket, and how
do bytes reach a connected BLE central. Each has one owner and one entry point, and they do not depend on each other:
Wi-Fi never needs BLE and the other way round. The crate is `no_std`, never allocates, has no ESP dependency and is
tested on a host.

*Wi-Fi connected* is not the same as *a TCP connection can be opened*: the control plane reports the link, the data plane
has its own readiness (link up and DHCP complete) and its own typed errors. Higher layers (TLS, HTTP, NTP, telemetry)
use `NetworkHandle` and standard embedded traits and never see `embassy-net` or a vendor type. The same radio side
appears in the management plane as the `wifi`, `network` and `ble` config sections and services, for example
`rohi config wifi` and `rohi status network` (see [Service pipeline](service.md)).

`rohi-net-esp` implements the backend traits on ESP32-C3 and ESP32-C6 on top of `esp-radio`, `embassy-net` and TrouBLE.
It owns the radio peripherals and the single fixed-size radio heap shared by Wi-Fi and BLE; the firmware decides how much
RAM sockets and buffers take ([Memory model](../concepts/memory-model.md),
[Resource limits](../reference/resource-limits.md)). It is empty without a chip feature
([Targets and features](../reference/targets-and-features.md)). How the pieces run together is in
[Wireless runtime](../platform/wireless.md); a product-level walkthrough is in [Use IP networking](../guides/use-network.md).

| Item | Role |
|------|------|
| `WifiManager`, `WifiHandle`, `WifiBackend` | Wi-Fi control plane: desired configuration, reconnection with backoff, scan, status |
| `NetworkHandle`, `IpBackend`, `UdpSocket`, `NetError` | IP data plane: DNS, TCP, UDP over `embedded-nal-async` and `embedded-io-async` |
| `BleTransport` | Bounded byte pipe to one BLE central (used by the control link) |
| `WifiService`, `NetworkService`, `BleService` | The `wifi`, `network` and `ble` services of the management plane |
| `EspWireless`, `EspWifi`, `EspIp`, `EspBle` (`rohi-net-esp`) | ESP32 backends and the shared radio runtime |

Status: implemented and host-tested; the ESP backend builds in CI for ESP32-C3 and ESP32-C6. Not hardware-verified yet
(see [Hardware testing](../validation/hardware-testing.md)). There is no TLS, SoftAP or IPv6, and WPA3-only networks
are not supported.

## Reference

The crate rustdoc is the authoritative technical documentation (configuration fields, reconciliation rules, errors,
limits, cancellation, testing with `FakeIp`):

- `rohi-net`: [docs.rs](https://docs.rs/rohi-net), [source](https://github.com/akagi-dev/rohi-sdk/tree/master/crates/net)
- `rohi-net-esp`: [docs.rs](https://docs.rs/rohi-net-esp), [source](https://github.com/akagi-dev/rohi-sdk/tree/master/crates/net-esp)
- SDK-level concepts: [docs.rs/rohi-sdk](https://docs.rs/rohi-sdk)
- Locally: `cargo doc --no-deps --open -p rohi-net` (the ESP crate needs a chip feature and target, see its docs)
