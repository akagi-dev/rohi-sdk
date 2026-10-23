# Runtime ownership

> **Rule.** Stateful drivers and services have **one mutable runtime owner**; other tasks interact with them through
> bounded handles, snapshots or commands.

There are no global mutable stores and no shared locks around drivers. The owner is an Embassy task (or, for the radio,
one runtime object); everything else holds a small copyable handle into a bounded, statically allocated channel.

| Service | Owner | Handle | Reached through |
|---------|-------|--------|-----------------|
| Measurements | store task owns `MeasurementStore` | `MeasurementHandle` | bounded snapshot request/response |
| Configuration | configuration task owns `ConfigStore` | `ConfigHandle` | bounded, numbered request/response |
| Wi-Fi | `WifiManager` task owns the Wi-Fi backend | `WifiHandle` | desired-state cell, bounded commands, published status |
| IP stack | the platform stack (`EspIp`) behind the stack runner task | `NetworkHandle` | DNS and TCP through standard traits |
| ESP wireless resources | `EspWireless` (radio peripherals, wireless heap) | the drivers it hands out once | `wifi()`, `ble()` |
| BLE peripheral | `BleRunner` task owns the controller and host stack | `EspBle` (`BleTransport`) | bounded RX/TX queues |

```text
MeasurementStore  ─ one owner task ─►  MeasurementHandle
ConfigStore       ─ one owner task ─►  ConfigHandle
WifiManager       ─ one owner      ─►  WifiHandle
ESP wireless resources ─ one owner / runtime ─► Wi-Fi + BLE
```

Why: a single owner makes ordering explicit (requests are served one at a time), keeps flash and radio access out of
timing-critical tasks, makes cancellation analysable (see [Async and cancellation](async-and-cancellation.md)) and lets
the owner be tested on the host with a mock backend.

## Consequences

- Nothing outside `altruist_urban::config` touches the configuration flash; nothing outside the Wi-Fi manager task calls
  the Wi-Fi driver; sensor tasks never touch the `MeasurementStore`.
- The owner never blocks on a caller that gave up: responses to abandoned requests are discarded.
- Handles are cheap (`Copy`) and are the only thing passed to other crates, which keeps portable crates independent of
  the owner's concrete types.
- Failure of one owner (for example Wi-Fi) is contained: the others keep running.

## State versus command

Two kinds of interaction exist; mixing them in one queue causes subtle bugs:

```text
desired persistent state  ──►  latest-state publication / reconciliation
transient requested action ──►  bounded command
```

> **Guideline.** Prefer state publication over queued commands for persistent desired state. Use commands for
> transient actions.

| Interaction | Kind | Mechanism |
|-------------|------|-----------|
| Desired `WifiConfig` | state | `WifiHandle::set_desired_config` (latest value wins, synchronous) |
| Latest system / Wi-Fi status | state | published `WifiStatus` (`Watch`) |
| Scan, reconnect, disconnect | command | bounded queue, `Busy` when full |
| One-shot request (snapshot, config read) | command | bounded request/response |

**Wi-Fi configuration is the canonical example.** The persistent configuration is *state*: the manager continuously
reconciles the actual radio state towards the latest desired value. Rapid updates A, B, C converge on C, an identical
update does nothing, and a new value invalidates the retry state of the old one. Had it been a command in the same FIFO
as `Scan`, a full queue would have forced the publisher to wait (a cancellable gap after the durable commit), updates
could not be coalesced and ordering against transient commands would matter. The reconciliation rules are in
[Networking](../sdk/net.md#reconciliation-behaviour).
