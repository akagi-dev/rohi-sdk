# Sensor Core

The Altruist Urban firmware turns physical sensors into immutable `MeasurementsSnapshot` values:

```text
physical sensor -> rohi-hal driver -> Runner (one Embassy task per sensor) -> SensorEvent
  -> bounded event queue -> store task -> MeasurementStore -> MeasurementsSnapshot
```

Dependency direction: `altruist-urban` → `rohi-hal` → `rohi-sensor-core`. Drivers know nothing about Wi-Fi, BLE,
serialization or any other transport, and nothing is averaged or fused across sensors: `bmx280.temperature` and
`scd4x.temperature` are separate observations. Per-sensor details are in [Sensors](sensors.md), pins in [Boards](boards.md).

## Runtime

The firmware runs on native Embassy, without an RTOS: `embassy-executor` in thread mode (`wfi` when idle) and a
SYSTIMER based `embassy-time` driver (1 MHz ticks, `rohi_hal::esp::time_driver`). Nothing allocates, so the firmware has
no heap. All waits use monotonic async timers; nothing depends on wall-clock time.

## Components

| Component | Location | Role |
|-----------|----------|------|
| Drivers | `crates/rohi-hal/src/sensors/` | BMx280, SCD4x, SDS011, noise. Hardware independent (`embedded-hal-async`, `embedded-io-async`), unit tested on a host |
| Runtime | `crates/rohi-hal/src/runtime/` | `Runner` lifecycle, `Reporter`, `EventBus`, backoff, bounded I²C |
| Board | `crates/rohi-hal/src/board/altruist_c3.rs` | All pin assignments, bus configuration (feature `esp`) |
| Tasks | `firmwares/altruist-urban/src/tasks/` | One Embassy task per sensor, single `MeasurementStore` owner |

Every sensor runs in its own task, so a failing or slow sensor never delays another one. Lightweight I²C sensors share
one bus; every transaction is bounded to 200 ms (`TimeoutI2c`), so a stuck device cannot block the bus forever.

## Sensor lifecycle

Each driver is wrapped by a `Runner` which maps driver results into `SensorState`:

```text
NotDetected -> Initializing -> WarmingUp -> Ready
Ready -> Degraded -> Error -> (re-init) -> Initializing -> Ready
```

* Absence is not an error: a sensor that never answered is `NotDetected` and probed every 10 s.
* A sensor that was detected before and stops answering is `Error` and recovered with exponential backoff, so a
  reconnected sensor returns to `Ready` without a reboot.
* The first failure gives `Degraded`; 3 consecutive failures give `Error` and driver re-initialization.
* Errors never erase the last valid observation; they are published as `SensorEvent::Error`.
* After a successful init a sensor is `WarmingUp` (SCD4x, SDS011) or still `Initializing` (BMx280, noise) until its
  first measurement arrives.

### Timing semantics

Four delays exist and are never added up:

| Delay | Owner | Meaning |
|-------|-------|---------|
| Initial delay | driver (`initial_delay_ms`, default 0) | Wait before the first sample after first detection. No current driver needs one |
| Poll interval | driver (`poll_interval_ms`) | Wait **after a successful** sample before the next one |
| Recovery backoff | runner (`RunnerConfig`) | Wait **after a failed** sample or init: 1 s, 2 s, 4 s … capped at 30 s; reset by the next successful *measurement* |
| Warm-up | driver, inside `sample` | Hardware specific: SDS011 fan spin-up, SCD4x first reading |

```text
success:  sample -> publish -> poll interval -> sample
failure:  sample -> error   -> backoff       -> sample   (no poll interval on top)
```

Retries therefore never wait "backoff + a full poll interval" (for the SDS011 that would be over 90 s), and the first
sample after boot is taken immediately.

## Events

Sensors publish three kinds of `SensorEvent`:

* `Measurements` — a successful observation;
* `StateChanged` — an actual lifecycle transition (published only when the state changes);
* `Error` — a failed operation.

**Success is one event.** The `MeasurementStore` derives `Ready` from a `Measurements` event: new values, their
timestamp and `Ready` are applied in one step, also when recovering from `Degraded` or `Error`. There is no separate
`StateChanged(Ready)`, so a consumer can never observe `Ready` together with an old value, and one successful cycle
costs one queue slot.

`EventBus` is a bounded queue of 16 events (`EVENT_QUEUE_CAPACITY`). When it is full, the **oldest** event is dropped
and counted (`EVENTS.dropped()`, logged with every snapshot): the newest state is more useful than a backlog and
producers never block. The store task is the only component mutating the `MeasurementStore`.

## Freshness

A valid measurement becomes `Stale` when it is older than the sensor's `FreshnessPolicy` (defaults in
`rohi-sensor-core`, tunable with `MeasurementStore::set_freshness_policy`). A host test in `rohi-hal` asserts that every
default exceeds the worst healthy cadence of its sensor by at least 1.5×:

| Sensor | Worst healthy cadence | Stale after |
|--------|-----------------------|-------------|
| BMx280 | 10 s | 60 s |
| SCD4x | 15 s | 30 s |
| SDS011 | 150 s (warm-up + sampling window + sleep) | 300 s |
| Noise | 10 s | 30 s |

Errors do not make a value stale; they mark it `Error` while the last valid value is preserved.

## SDS011 and noise coordination

The legacy firmware coordinated SDS011 activity with noise measurement (the fan is a noise source). This is not
implemented. The runtime exposes `STATES` (`SensorStates`), a read-only view of every sensor state, so a future noise
policy can react to the SDS011 state (e.g. flag windows while it is in `WarmingUp`) without coupling the drivers.

## Testing

Host tests (no hardware needed):

```bash
cargo test-host        # alias for: cargo test -p rohi-sensor-core -p rohi-hal --target x86_64-unknown-linux-gnu
cargo check --workspace
cargo build --release -p altruist-urban
```

The host tests cover the runner timing and recovery, event consistency, the store, SDS011 sampling and aggregation,
BMx280 address probing and the freshness thresholds.

Hardware examples (each prints typed measurements, states and errors):

```bash
cargo run --release --bin example-bmx280
cargo run --release --bin example-scd4x
cargo run --release --bin example-sds011
cargo run --release --bin example-noise
cargo run --release --bin example-all-sensors   # combined test
```

Combined test checklist: BMx280 keeps updating; SCD4x keeps updating; SDS011 completes its duty cycle and sleeps;
noise windows keep arriving; no sensor starves another. Hot-reconnect check: unplug and replug an I²C sensor and
confirm `Error → Initializing → Ready` without a reboot; repeat for the SDS011 UART.

## Resource baseline

`cargo build --release -p altruist-urban`, ESP32-C3 (before Wi-Fi/BLE/TLS):

| Item | Size |
|------|------|
| `.text` (flash) | 114 370 B |
| `.rodata` (flash) | 24 096 B |
| `.rwtext` + `.data` (RAM) | 1 700 B + 2 132 B |
| `.bss` (RAM, includes task futures) | 9 160 B |
| Heap | none (no allocator) |
| Event queue | 16 events |
| Noise buffers | 256 samples (1 KiB) stack + 8 KiB DMA ring |

Task stack usage is not measured yet.

## Stability test

Run `altruist-urban` without networking for at least 24 h (preferably 72 h) with all sensors attached and record the
following. The table is filled in after each hardware run:

| Item | Result |
|------|--------|
| Duration | |
| Unexpected reboots / panics | |
| Sensor errors per sensor | |
| Successful recoveries | |
| `dropped events` (log) | |
| Task stalls (snapshots stop updating) | |
| Stale measurements | |
| Memory stability | |
