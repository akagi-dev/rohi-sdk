# Runtime ownership

> **Rule.** Stateful drivers and services have **one mutable runtime owner**; other tasks interact with them through
> bounded handles, snapshots or commands.

There are no global mutable stores and no shared locks around drivers. The owner is an Embassy task (or, for the radio,
one runtime object); everything else holds a small copyable handle into a bounded, statically allocated channel. A
single owner makes ordering explicit, keeps flash and radio access out of timing-critical tasks, makes cancellation
analysable ([Async and cancellation](async-and-cancellation.md)) and lets the owner be tested on the host with a mock
backend. A failing owner (for example Wi-Fi) does not stop the others.

| Service | Owner | Handle |
|---------|-------|--------|
| Measurements | store task | `MeasurementHandle` |
| Configuration | configuration task of the firmware | `ConfigHandle` |
| Wi-Fi | `WifiManager` task | `WifiHandle` |
| IP stack | platform stack behind its runner task | `NetworkHandle` |
| ESP wireless resources | `EspWireless` | the drivers it hands out once |

The rustdoc has the complete owner table, including the BLE peripheral, device identity and telemetry.

## State versus command

Persistent *desired state* (for example the desired `WifiConfig`) is published as a latest value that the owner keeps
reconciling towards; transient actions (scan, reconnect, a one-shot request) are bounded commands. Mixing both in one
queue causes subtle bugs, so prefer state publication over queued commands for persistent desired state. The Wi-Fi
configuration is the canonical example; its reconciliation rules are in
[`WifiManager`](https://docs.rs/rohi-net) rustdoc (see also [Networking](../sdk/net.md)).

## Reference

- [`rohi_sdk::ownership`](https://docs.rs/rohi-sdk/latest/rohi_sdk/ownership/index.html): owner and handle table,
  consequences, state versus command.
- Source: [`crates/sdk`](https://github.com/akagi-dev/rohi-sdk/tree/master/crates/sdk).
- Locally: `cargo doc --no-deps --open -p rohi-sdk`.
