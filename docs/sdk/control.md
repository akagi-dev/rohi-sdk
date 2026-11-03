# Control plane (BLE)

The control plane lets a client on the other side of a Bluetooth LE link inspect and configure a ROHI device locally.
It is **generic**: the protocol and the dispatcher know no module by name. Four planes, all driven by schemas the
device publishes, cover the device **info**, persistent **config**, live **status** and explicit **actions**. What
those planes contain (Wi-Fi, telemetry, sensors, identity, location, ...) is decided by the firmware's registries, see
[Service pipeline](service.md). The scope is **local control only**: uplink telemetry, HTTP and SoftAP are not part of
it. For the idea behind it, see [Control protocol](../build/control-protocol.md).

Three portable `no_std` crates and one client split the work. `rohi-control-proto` is the wire contract: a bounded
protobuf schema with compile-time size limits. `rohi-control` is the dispatcher: it decodes one request, checks it
against an authorization policy, serves it from the `rohi-service` registries and encodes one response. `rohi-control-ble`
is the **firmware-side** BLE peripheral part: GATT identifiers, framing of messages into chunks, and the loop that
serves a connected central. It is not a BLE client; the host `rohi` tool ([Desktop application](../use/desktop/index.md))
is the central and reuses only its frame and UUID modules. The BLE hardware itself sits behind the `BleTransport` trait
of `rohi-net` (see [Crate map](../concepts/crate-map.md)).

Status: the protocol, dispatcher and framing are covered by host tests (including the `rohi` client end to end). BLE on
real hardware is **not verified** (see [Hardware testing](../validation/hardware-testing.md)). **Pairing and
encryption are not implemented yet**: the Altruist Urban firmware runs the open policy, so anyone in radio range can
use the control plane, including identity provisioning and erase. The protocol is under development: keep the tool
and the firmware on the same revision.

| Crate / item | Role |
|--------------|------|
| `rohi-control-proto` | `control.proto`, generated bounded message types, `PROTOCOL_VERSION`, `limits`, `encode`/`decode` |
| `rohi-control` | `Controller`, `ControlBackend` / `Device`, `AuthorizationPolicy` (`OpenPolicy`, `RequireEncryption`, `RequireAuthenticated`) |
| `rohi-control-ble` | `ControlBle` serving loop, `frame` (fragmenter, reassembler), `uuid` (GATT service and characteristics) |

The authoritative protocol, its error codes and its size limits are the rustdoc of
[`rohi-control-proto`](https://docs.rs/rohi-control-proto) and `control.proto`; the limits are defined once in that
crate's `build.rs`. Resource figures: [Resource limits](../reference/resource-limits.md#control-protocol) and
[Resource budget](../validation/resource-budget.md#control-protocol-types). The shared radio runtime is described in
[Wireless runtime](../platform/wireless.md).

## Reference

- docs.rs: [`rohi-control-proto`](https://docs.rs/rohi-control-proto), [`rohi-control`](https://docs.rs/rohi-control),
  [`rohi-control-ble`](https://docs.rs/rohi-control-ble); SDK level: [`rohi-sdk`](https://docs.rs/rohi-sdk)
- Source: [control-proto](https://github.com/akagi-dev/rohi-sdk/tree/master/crates/control-proto),
  [control](https://github.com/akagi-dev/rohi-sdk/tree/master/crates/control),
  [control-ble](https://github.com/akagi-dev/rohi-sdk/tree/master/crates/control-ble)
- Local build: `cargo doc --no-deps --open -p rohi-control-proto` (also `-p rohi-control`, `-p rohi-control-ble`)
- Adding an operation: [Add a control operation](../guides/add-control-operation.md)
