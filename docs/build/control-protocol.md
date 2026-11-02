# Control protocol

The **control protocol** is the shared contract that lets a generic application operate any firmware. It exists so
that a firmware author describes a device once, in [Config, Status and Action](core-concepts.md), and every client can
use it without a firmware-specific integration.

```text
 Firmware        sections · services · actions (rohi-service)
    │            described by schemas the device publishes
 Protocol        request/response messages (protobuf), one schema, bounded sizes
    │
 Transport       moves bytes: BLE today (GATT), others possible
    │
 Application     rohi desktop · mobile app (planned) · your client
```

## What it does

- **Makes capabilities discoverable.** Clients ask what sections, services and actions exist and fetch their schemas. Config
  becomes readable and editable settings, Status becomes health and metrics, Action becomes callable operations.
- **Avoids per-firmware clients.** The protocol, the dispatcher and the client know no module by name, so a new
  firmware module needs no client change.
- **Separates concerns.** The firmware owns meaning, the protocol owns the wire format and the transport owns delivery.
  The transport carries opaque bytes, so replacing it does not change the firmware or the clients.

## Current state

| Aspect | State |
|--------|-------|
| Protocol | version 1; **still under development** and changing without a version bump: keep client and firmware on the same revision |
| Transport | BLE is the only implemented transport; the `tcp://` device URI is parsed but not implemented |
| Security | the link is not paired or encrypted yet; the policy hook exists, the reference firmware runs the open policy |
| Scope | local control only; telemetry uplink is a separate service |

## Where to go next

- Operate a device: [Desktop application](../use/desktop/index.md), [Mobile application](../use/mobile.md) (planned).
- The authoritative definition is the schema [`control.proto`](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/control-proto/proto/rohi/control/v1/control.proto)
  and the rustdoc of `rohi-control-proto`, `rohi-control` and `rohi-control-ble`, see [SDK crates](sdk-crates.md).
- Planes, framing, limits and security in detail: [Control plane (BLE)](../sdk/control.md).
- Extend the protocol: [Add a control operation or an action](../guides/add-control-operation.md).
