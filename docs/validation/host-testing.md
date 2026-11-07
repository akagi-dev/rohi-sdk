# Host testing

Most of the SDK is testable without hardware. This page defines the test pyramid, what should normally be proven on the
host and the cancellation test pattern.

```text
host unit tests
        ↓
host service / integration tests
        ↓
ESP compile checks
        ↓
hardware functional validation        ← Hardware testing
        ↓
long-running stability tests          ← Hardware testing
```

| Level | Command | What it proves | Hardware needed |
|-------|---------|----------------|-----------------|
| Host unit tests | `cargo test` | codecs, state machines, bounds, error mapping, golden encodings | no |
| Host service / integration | `cargo test` | owner + handle + mock backend: reconnect logic, reconciliation, dispatcher, framing, `rohi` client end to end against the real dispatcher, cancellation | no |
| `std` platform end to end | `cargo test -p altruist-urban --features std,std-tcp,sensor-simulator` | the composed application on the host (sensors, storage, Wi-Fi, TCP control, restarts) driven through the real `rohi` client; see [Run Altruist Urban on std](../guides/altruist-urban-std.md). The BLE peripheral is tested without an adapter; a real adapter and phone are a manual [smoke test](../guides/linux-ble.md), not hardware validation | no |
| ESP compile checks | `cargo check-esp32c3`, `cargo check-esp32c6`, `cargo build-esp32c3/6`, ESP `clippy` | the platform crates and firmware type-check and link for both chips | no |
| Hardware functional validation | flash + examples/firmware | radios, flash, buses, pins, heap high-water mark | **yes** ([Hardware testing](hardware-testing.md)) |
| Long-running stability | 24–72 h soak | leaks, deadlocks, recovery under churn | **yes** |

## What should be testable without hardware

Everything in the portable crates, behind its seam trait:

- sensor drivers and their runner (mock buses, recording delay);
- the configuration store, including power-fail safety (injected write and erase faults at every byte);
- the Wi-Fi manager and the IP data plane (scripted backends, no radio, no Internet);
- the time service and the NTP source (fake clock and sources);
- the control protocol, dispatcher, framing and the `rohi` client (in-memory transport);
- cancellation of all of the above.

What the host cannot prove: radio and coexistence behaviour, flash timing, DMA, pin maps, real heap use. If a bug is
found in one of those, add a host test for the logic part and record the hardware check.

## Fakes and mocks

Every hardware seam has a host implementation (in-memory storage, mock radios, a fake IP stack, scripted BLE transports,
mock buses), and the whole composed application runs on the host as the [`std` Altruist Urban](../guides/altruist-urban-std.md); the seams and their fakes are described in the [portability rustdoc](https://docs.rs/rohi-sdk/latest/rohi_sdk/portability/index.html).

## Cancellation tests

For every public async operation with non-trivial cancellation semantics
([Async and cancellation](https://docs.rs/rohi-sdk/latest/rohi_sdk/cancellation/index.html)) test the hazard by starting the operation, polling it once and dropping it while pending:

```text
start the operation ── poll once (pending) ── drop it ── then verify:
  - the service still responds
  - channel / slot / pool capacity is not permanently lost
  - a stale answer is not delivered to the next caller
  - nothing stays locked
  - the next operation succeeds
```

Existing tests cover dropped Wi-Fi scans, configuration changes while connecting, cancelled network operations,
cancelled control requests and dropped sensor steps; find them with `cargo test cancel`.

## Book examples

Rust code shown in the book is either compiled by `cargo test` and included with `\{{#include file:anchor}}` (for example
`crates/net/tests/network_example.rs`) or marked `ignore`/`text` when it cannot run on the host (hardware snippets).
`mdbook test docs` runs plain snippets. See [Documentation style and policy](../contributing/docs-style.md).
