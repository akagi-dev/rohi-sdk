# Networking (`rohi-net`)

`rohi-net` is the portable networking layer of the SDK. It is organised in three layers; each has one job and one
entry point:

```text
 application / control plane / future uplink (TLS, HTTP)
        │                              │
   WifiHandle                    NetworkHandle
   Wi-Fi control plane           IP data plane
   configure · scan · status     DNS · TCP
   reconnect · disconnect        embedded-nal-async + embedded-io-async
        │                              │
   WifiManager  ── WifiBackend    IpBackend
        │                              │
        └──────────── rohi-net-esp ────┘      ESP backend: EspWifi, EspIp
                           │
              esp-radio · embassy-net · esp-rtos · radio heap
```

| Layer | Question it answers | Owner |
|-------|---------------------|-------|
| **Wi-Fi control plane** (`WifiHandle`) | Which network should the radio use, and is it connected? | `WifiManager` (one task) |
| **IP data plane** (`NetworkHandle`) | Can I resolve a name and open a TCP connection? | the platform IP stack (`EspIp`) |
| **ESP backend** (`rohi-net-esp`) | How are the radio and the IP stack driven on ESP32-C3 / C6? | `EspWireless` |

*Wi-Fi connected* is **not** the same as *a TCP connection can be opened*. The control plane reports link state; the
data plane has its own readiness (link up and DHCP complete) and its own typed errors. Higher-level crates must use
`NetworkHandle` (or the standard traits it implements) and never touch `embassy-net` or any vendor type directly:
no `esp-radio`, `esp-hal` or `embassy-net` type appears in a public signature of `rohi-net`; vendor types and errors are
converted inside `rohi-net-esp`. `rohi-net` has no ESP dependency and builds and tests on the host.

## Wi-Fi control plane

`WifiHandle` is a cheap, copyable handle to the `WifiManager`, the single owner of the Wi-Fi backend. It does **not**
carry DNS, sockets, TLS or HTTP.

| Type | Role |
|------|------|
| `WifiConfig` | Persistent CBOR section `WifiConfig` (`ssid`, optional `password`); open network = `password: None` |
| `WifiState` / `WifiStatus` | `NotConfigured`, `Idle`, `Scanning`, `Connecting`, `Connected`, `Disconnected`, `Error` (+ last `WifiError`) |
| `NetworkInfo` | SSID, RSSI, IPv4, gateway, DNS. Never contains credentials |
| `WifiNetwork` / `WifiScanResults` | Bounded scan result (`heapless::Vec`), strongest first |
| `WifiError` | Stable typed errors (`AuthenticationFailed`, `DhcpFailed`, `Busy`, ...) |
| `WifiShared` / `WifiHandle` | `static` shared state and the handle other tasks use |

`Connected` means *network ready*: association **and** DHCP have completed.

### Desired state versus commands

The persistent configuration is **state**, the rest are **transient actions** (see
[State versus command](../concepts/runtime-ownership.md#state-versus-command)):

```text
persistent WifiConfig ──► desired Wi-Fi config ──► WifiManager ──► actual Wi-Fi state
                           (latest value wins)      reconciles
Scan · Reconnect · Disconnect ──► bounded command queue ──► WifiManager
```

- `WifiHandle::set_desired_config(Option<WifiConfig>)` publishes the desired configuration. It is **synchronous**: no
  `.await`, no queue, never `Busy`. The latest value wins; updates published before the manager ran are coalesced, so
  rapid updates A, B, C converge directly on C instead of causing a connect/disconnect cycle for A and B.
- `scan()`, `reconnect()` and `disconnect()` are commands in a bounded queue (`WIFI_COMMAND_QUEUE_DEPTH`). They return
  `Busy` when the queue is full, and only one scan can be in flight.
- Who publishes: the firmware's configuration owner task publishes the committed value right after the commit and once at
  boot, so a cancelled caller cannot leave a stored configuration unknown to the runtime
  ([Async and cancellation](../concepts/async-and-cancellation.md#durable-mutation)).

### Reconciliation behaviour

| Desired update | Behaviour |
|----------------|-----------|
| `None` | disconnect if necessary, `NotConfigured`, **no** automatic reconnect |
| new `Some(config)` | abort the running attempt or backoff, disconnect, connect with the new config and a **fresh backoff** |
| same config again | nothing: attempt, backoff timer, idle state or link continue (use `reconnect` to force a new attempt) |
| invalid config | disconnect, `NotConfigured` with `WifiError::InvalidConfig` |

The same rules hold in every phase (connecting, backing off, connected, idle). An update published while a scan runs
is applied after the scan. A new configuration invalidates retry state of the previous one: if AP A had a wrong
password and a 60 s backoff, AP B does not inherit the remaining wait.

Other behaviour:

- No configuration: state `NotConfigured`, no retry loop.
- Failure or link loss: bounded exponential backoff (1 s doubling up to 60 s, see
  [Resource limits](../reference/resource-limits.md#wi-fi)). The backoff is reset only after the link stayed up for one
  info refresh interval (10 s), so a flapping link keeps backing off. Failures never panic or reboot; sensors keep
  running.
- Scans never abort or delay reconnection: a scan requested while connecting is served after the attempt, and a scan
  during the backoff wait or while connected does not restart the backoff or refresh timer.

## IP data plane

`NetworkHandle<'a, B: IpBackend>` is a copyable handle to the IP stack. It implements the standard embedded Rust
traits, so TLS, HTTP and uplink layers stay independent of `embassy-net`:

| Trait | Provided by `NetworkHandle` |
|-------|-----------------------------|
| `embedded_nal_async::Dns` | `get_host_by_name` (IPv4); reverse lookup is not supported |
| `embedded_nal_async::TcpConnect` | `connect` → a connection that implements `embedded_io_async::{Read, Write}` |

```rust,ignore
{{#include ../../crates/rohi-net/tests/network_example.rs:fetch}}
```

This function is compiled and run against the host fake backend by `cargo test -p rohi-net`
(`tests/network_example.rs`). On the device the same function runs against the firmware's handle; see
[Use IP networking](../guides/use-network.md).

### Readiness and errors

Every operation first checks readiness (link up **and** DHCP complete) and fails with `NetError::NetworkUnavailable`
while Wi-Fi is not configured or connecting, DHCP is incomplete or the link was lost, also when readiness is lost
during the operation. Higher layers can therefore tell an unusable network from a DNS or connect failure while the
network is up.

| `NetError` | Meaning |
|------------|---------|
| `NetworkUnavailable` | Wi-Fi not configured/connecting, DHCP incomplete, link lost |
| `DnsFailed` | Resolution failed while the network was up |
| `ConnectFailed` | Connection refused or unreachable while the network was up |
| `NoSocketAvailable` | All statically allocated TCP connections are in use |
| `ConnectionReset` | An established connection was reset or closed |
| `Timeout` | A timeout expired |
| `Backend` | Unexpected stack failure |

`NetError` implements `embedded_io_async::Error`; raw `embassy-net` errors never reach higher layers.

### Socket resources

TCP connections are **statically bounded**; nothing allocates. Opening more simultaneous connections than the product
configured fails **immediately** with `NetError::NoSocketAvailable`: `connect` never waits for a free slot. A slot is
released when the connection is dropped. The capacity is a product decision, see
[Resource limits](../reference/resource-limits.md#ip-data-plane).

### Cancellation

All `NetworkHandle` operations are cancel-safe: dropping `get_host_by_name` allocates nothing; dropping a pending
`connect` aborts the attempt and frees its slot; dropping a read or write future transfers nothing that was not
reported. See [Async and cancellation](../concepts/async-and-cancellation.md).

### Testing without hardware

`rohi_net::testing::FakeIp` (feature `testing`) is an in-memory `IpBackend`: scripted DNS answers, refused connections,
readiness changes, slot exhaustion, held operations for cancellation tests and the peer side of every connection. See
[Host testing](../validation/host-testing.md).

## ESP backend (`rohi-net-esp`)

The only platform-specific entry point is `EspWireless` (see [Wireless runtime](../platform/wireless.md)), called once by
the board firmware. It owns the radio peripherals and the single radio heap shared with BLE and creates:

- `EspWifi` (implements `WifiBackend`) and its `NetRunner` (the background task of the IP stack),
- `EspIp` (implements `IpBackend`): DNS and TCP on the same `embassy-net` stack.

The firmware, not the backend, decides how much RAM this takes: it passes a `NetResources` value (stack sockets, TCP
connections, buffer sizes) and the heap size. See [Memory model](../concepts/memory-model.md).

The `embassy-net` features are limited to what a current component uses: **DHCPv4, DNS, TCP** and the Ethernet medium
used by `esp-radio`. UDP is not enabled. Policy: platform and network protocol features are enabled only when a current
SDK component uses them; later work adds features together with the component that needs them
([Targets and features](../reference/targets-and-features.md#network-stack-features)).

## Memory policy

Application and runtime code is heapless. The Espressif wireless stack needs dynamic allocation, so the ESP backend
uses a **fixed-size** wireless heap (`esp-alloc`, no growth) that Wi-Fi and BLE share. Heap-backed vendor results (for
example the scan `Vec`) are converted into bounded ROHI types inside the backend and dropped there. See
[Memory model](../concepts/memory-model.md).

## Security

The Wi-Fi password is secret: `WifiPassword` has a redacting `Debug`, nothing logs configuration contents, and
`NetworkInfo` has no credential field. Only the backend reads the secret (`expose_secret`). The control API (see
[Control plane](control.md)) never returns the password and accepts it write-only. The TCP layer has no TLS: do not
send secrets over `NetworkHandle` without a TLS layer on top.

## Out of scope

SoftAP, TLS/HTTP(S), NTP, Connectivity Protocol uplink, multiple saved networks, static IP, WPA enterprise.
`WifiConfig` authentication currently selects WPA/WPA2 personal; WPA3-only networks are not yet supported by the
`esp-radio` station configuration used here.

Hardware validation status of Wi-Fi, IP and coexistence is tracked in
[Hardware testing](../validation/hardware-testing.md).
