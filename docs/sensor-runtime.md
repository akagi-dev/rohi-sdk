# Sensor runtime

The Altruist Urban firmware turns physical sensors into immutable `MeasurementsSnapshot` values:

```text
physical sensor -> rohi-hal driver -> async sensor task -> SensorEvent
  -> bounded event queue -> store task -> MeasurementStore -> MeasurementsSnapshot
```

Dependency direction: `altruist-urban` → `rohi-hal` → `rohi-sensor-core`. Drivers know nothing about Wi-Fi, BLE,
serialization or any other transport, and nothing is averaged or fused across sensors: `bmx280.temperature` and
`scd4x.temperature` are separate observations.

## Runtime

The firmware runs on native Embassy, without an RTOS: `embassy-executor` in thread mode (`wfi` when idle) and a
SYSTIMER based `embassy-time` driver (1 MHz ticks, `rohi_hal::esp::time_driver`). Nothing allocates, so the firmware has no heap.

## Components

| Component | Location | Role |
|-----------|----------|------|
| Drivers | `crates/rohi-hal/src/sensors/` | BMx280, SCD4x, SDS011, noise. Hardware independent (`embedded-hal-async`, `embedded-io-async`), unit tested on a host |
| Runtime | `crates/rohi-hal/src/runtime/` | `Runner` lifecycle, `Reporter`, `EventBus`, backoff, bounded I²C |
| Board | `crates/rohi-hal/src/board/altruist_c3.rs` | All pin assignments, bus configuration (feature `esp`) |
| Tasks | `firmwares/altruist-urban/src/tasks/` | One Embassy task per sensor, single `MeasurementStore` owner |

## Sensor lifecycle

Each driver is wrapped by a `Runner` which maps results into `SensorState`:

```text
NotDetected -> Initializing -> WarmingUp -> Ready
Ready -> Degraded -> Error -> (re-init) -> Initializing -> Ready
```

* Absence is not an error: a sensor that never answered is `NotDetected` and probed every 10 s.
* A sensor that was detected before and stops answering is `Error` and recovered with exponential backoff
  (1 s … 30 s), so a reconnected sensor returns to `Ready` without a reboot.
* First failure → `Degraded`; 3 consecutive failures → `Error` and driver re-initialization.
* Errors never erase the last valid observation; they are published as `SensorEvent::Error`.

## Event queue

`EventBus` is a bounded queue of 16 events (`EVENT_QUEUE_CAPACITY`). When it is full, the **oldest** event is dropped
and counted (`EVENTS.dropped()`, logged with every snapshot): newest state is more useful than a backlog and producers
never block. The store task is the only component mutating the `MeasurementStore`.

## Timing

| Sensor | Cadence | Details |
|--------|---------|---------|
| BMP280 / BME280 | 10 s | Forced mode: T ×2, P ×4, H ×1, filter off, ≈19 ms per conversion, I²C `0x76` |
| SCD4x | 5 s | Periodic measurement, data-ready polling every 500 ms (max 20 polls), CRC-8 on every word |
| SDS011 | ≈125 s cycle | Query mode: 30 s warm-up, one query, 90 s sleep; see below |
| Noise | 10 s window | 16 kHz mono; see below |

All waits use monotonic async timers; nothing depends on wall-clock time. Every I²C transaction is bounded to 200 ms
(`TimeoutI2c`), every UART/I²S read to SDS011 5 s / 1 s (I²S).

### SDS011 cycle

The command flow and wire protocol (`sensors/sds011/protocol.rs`) are derived from the community
[`sds011-rs`](https://github.com/kenohassler/sds011-rs) driver (MIT OR Apache-2.0). The sensor is kept in query mode and
asleep between cycles: `wake → warm-up (30 s) → query → sleep`. On top of that the driver bounds every UART read to 5 s,
discards stale bytes (e.g. frames of a sensor still in active mode), skips unexpected frames, retries the wake-up up to
3 times, puts the sensor to sleep if a session fails, and rejects values above 999.9 µg/m³. One sample per cycle is
reported. A silent sensor leads to `Degraded` → `Error` → re-initialization.

### Noise

* 16 kHz, 24-bit samples (ICS-43434 assumed: −26 dBFS at 94 dB SPL, so full scale is 120 dB SPL), unweighted dB SPL.
* ≈10 Hz integer DC blocker, level every 125 ms sub-window; per 10 s window *average* is the energy average
  (equivalent continuous level) and *maximum* the highest sub-window level.
* Raw audio is never stored: a 256-sample stack chunk plus the 8 KiB DMA ring buffer (≈128 ms) are the only buffers.

### SDS011 and noise coordination

The legacy firmware coordinated SDS011 activity with noise measurement (the fan is a noise source). This could not be
verified on current hardware here, so no coordination is implemented. The runtime exposes `STATES`
(`SensorStates`), a read-only view of every sensor state, so a future noise acquisition policy can react to the
SDS011 state (e.g. flag windows while it is in `Warmup`) without coupling the drivers.

## Pins

All GPIO assignments live in `board/altruist_c3.rs`. I²C (GPIO3/GPIO0) and SDS011 UART (GPIO10/GPIO1) come from the
previous firmware; **the I²S pins (GPIO4/5/6) are PLACEHOLDERS** that must be replaced with the real hardware assignment.

## Testing

Host tests (no hardware needed):

```bash
cargo test-host        # alias for: cargo test -p rohi-sensor-core -p rohi-hal --target x86_64-unknown-linux-gnu
cargo check --workspace
cargo build --release -p altruist-urban
```

Hardware examples (each prints typed measurements, states and errors):

```bash
cargo run --release --bin example-bmx280
cargo run --release --bin example-scd4x
cargo run --release --bin example-sds011
cargo run --release --bin example-noise
cargo run --release --bin example-all-sensors   # combined test
```

Combined test checklist: BMX280 keeps updating; SCD4x keeps updating; SDS011 cycles correctly; noise windows keep
arriving; no sensor starves another. Hot-reconnect check: unplug/replug an I²C sensor and confirm
`Error → Initializing → WarmingUp/Ready` without a reboot.

## Resource baseline

`cargo build --release -p altruist-urban`, ESP32-C3 (before Wi-Fi/BLE/TLS):

| Item | Size |
|------|------|
| `.text` (flash) | 112 744 B |
| `.rodata` (flash) | 23 768 B |
| `.rwtext` + `.data` (RAM) | 1 700 B + 2 104 B |
| `.bss` (RAM, includes task futures, DMA ring buffer) | 15 360 B |
| Heap | none (no allocator) |
| Event queue | 16 events |
| Noise buffers | 256 samples (1 KiB) stack + 8 KiB DMA ring |

Task stack usage is not measured yet.

## Stability test

Run `altruist-urban` without networking for at least 24 h (preferably 72 h) with all sensors attached and record:

| Item | Result |
|------|--------|
| Duration | |
| Unexpected reboots / panics | |
| Sensor errors per sensor | |
| Recovery events | |
| `dropped events` (log) | |
| Memory stability | |
| Task stalls (snapshots stop updating) | |
