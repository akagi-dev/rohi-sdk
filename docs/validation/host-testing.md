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
| ESP compile checks | `cargo check-esp32c3`, `cargo check-esp32c6`, `cargo build-esp32c3/6`, ESP `clippy` | the platform crates and firmware type-check and link for both chips | no |
| Hardware functional validation | flash + examples/firmware | radios, flash, buses, pins, heap high-water mark | **yes** ([Hardware testing](hardware-testing.md)) |
| Long-running stability | 24–72 h soak | leaks, deadlocks, recovery under churn | **yes** |

## What should be testable without hardware

Everything in the portable crates, behind its seam trait:

- sensor drivers and the runner (mock I²C/UART, recording delay);
- the configuration store, including power-fail safety (`MemoryStorage` fault injection: every torn write and erase);
- the Wi-Fi manager and reconciliation (mock `WifiBackend` with scripted outcomes, held connects/scans/delays);
- the IP data plane (`rohi_net::testing::FakeIp`: readiness, DNS, refused connections, slot exhaustion, read/write, UDP);
- the time service (`rohi-time`: fake clock and fake sources) and the NTP source (`rohi-time-ntp`: a scripted NTP server over `FakeIp`, no Internet);
- the control protocol, dispatcher, framing and the `rohi` client (in-memory transport);
- cancellation of all of the above.

What the host cannot prove: radio and coexistence behaviour, flash timing, DMA, pin maps, real heap use. If a bug is
found in one of those, add a host test for the logic part and record the hardware check.

## Fakes and mocks

| Seam | Host implementation |
|------|---------------------|
| `ConfigStorage` | `rohi_config::MemoryStorage` |
| `WifiBackend` | mock in `rohi-net/src/wifi/manager/tests.rs` |
| `IpBackend` | `rohi_net::testing::FakeIp` (feature `testing`) |
| `BleTransport` | scripted fake in `rohi-control-ble` tests; in-memory transport in `tools/rohi` |
| `ControlBackend` / registries | `Device` built from fake services in `rohi-control` and `rohi-control-ble` tests; the real sections and services in `tools/rohi` tests |
| sensor buses and delays | `MockI2c`, `RecordingDelay`, `FixedClock` in `rohi-hal` |

## Cancellation tests

For every public async operation with non-trivial cancellation semantics
([Async and cancellation](../concepts/async-and-cancellation.md)) test the hazard with `embassy_futures::poll_once` and a
pinned future that is dropped while pending:

```text
start the operation ── poll once (pending) ── drop it ── then verify:
  - the service still responds
  - channel / slot / pool capacity is not permanently lost
  - a stale answer is not delivered to the next caller
  - nothing stays locked
  - the next operation succeeds
```

Existing examples: dropped Wi-Fi scan (`dropped_scan_does_not_leak_into_the_next_one`), configuration change during
connecting/backoff/scanning, dropped measurement snapshot, cancelled `connect` / read / write / DNS on `FakeIp`,
cancelled control request (`cancelled_step_abandons_the_request_and_the_next_one_works`), dropped sensor step during
its warm-up wait.

## Book examples

Rust code shown in the book is either compiled by `cargo test` and included with `\{{#include file:anchor}}` (for example
`crates/net/tests/network_example.rs`) or marked `ignore`/`text` when it cannot run on the host (hardware snippets).
`mdbook test docs` runs plain snippets. See [Documentation style and policy](../contributing/docs-style.md).
