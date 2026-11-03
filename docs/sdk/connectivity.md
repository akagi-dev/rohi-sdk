# Telemetry (`rohi-connectivity`)

`rohi-connectivity` sends the measurements of a device to a telemetry ingress. It turns an immutable
`MeasurementsSnapshot` into a signed [`airalab/connectivity-protocol`](https://github.com/airalab/connectivity-protocol)
frame and delivers it through a pluggable transport; HTTP over `rohi-net` is the transport implemented today (plain TCP,
no TLS). Signed telemetry is implemented: the signature comes from the device identity ([Identity](identity.md)), the
timestamp from synchronized UTC ([Time](time.md)) and the frame may carry the device position ([Location](location.md)).

The crate owns only the uplink path. It does not sense (that is [`rohi-hal`](hal.md)), keep the identity, synchronize the
clock or know where the device is: it consumes those through small traits. Sensing never depends on it: the service only
reads snapshots, so a Wi-Fi, DNS, server, UTC or authorization failure cannot stop or delay a sensor. A frame is encoded
and signed once and a retry resends the same bytes; the outbox is bounded and in memory (frames are lost on reboot).

Telemetry is configured by the `connectivity` config section (endpoint, node id, interval) with the generic
`rohi config` commands and reported by the `connectivity` status service; it is disabled until an endpoint is set
([Service pipeline](service.md)). Its limits are in [Resource limits](../reference/resource-limits.md).
`rohi-connectivity-proto` holds the bounded, allocation-free Rust bindings of the wire format; the protocol revision is
pinned and its `.proto` files are vendored in the crate.

| Item | Role |
|------|------|
| `UrbanEncoder` | Snapshot to `core.v1.Message`; only valid measurements, unsupported sensors omitted and counted |
| `EnvelopeBuilder`, `EnvelopeFrame`, `FrameSigner` | Signed, immutable `crypto.v1.SignedEnvelope` |
| `ConnectivityService`, `Outbox` | One task: create, queue and deliver frames with retry and backoff |
| `EnvelopeTransport`, `HttpTransport` | Transport trait and `POST /v1/telemetry` implementation |
| `ConnectivityConfig`, `ConnectivityStatusService` | The `connectivity` section and its service |
| `rohi-connectivity-proto` | Generated messages, `encode` / `decode`, `signing_preimage`, `limits` |

Status: implemented and host-tested, including interoperability vectors generated with the `robins` reference
implementation; the Altruist Urban firmware builds in CI. Delivery over real Wi-Fi to a real ingress is not
hardware-verified ([Hardware testing](../validation/hardware-testing.md)).

## Reference

The crate rustdoc is the authoritative technical documentation (measurement mapping, wire contract, error
classification, retry rules, configuration and status tables):

- `rohi-connectivity`: [docs.rs](https://docs.rs/rohi-connectivity), [source](https://github.com/akagi-dev/rohi-sdk/tree/master/crates/connectivity)
- `rohi-connectivity-proto`: [docs.rs](https://docs.rs/rohi-connectivity-proto), [source](https://github.com/akagi-dev/rohi-sdk/tree/master/crates/connectivity-proto)
- SDK-level concepts: [docs.rs/rohi-sdk](https://docs.rs/rohi-sdk)
- Locally: `cargo doc --no-deps --open -p rohi-connectivity -p rohi-connectivity-proto`
