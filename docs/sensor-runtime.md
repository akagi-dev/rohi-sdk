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

* `Ready` is entered **only by a measurement**: a successful sample is published as a single `Measurements` event and
  the `MeasurementStore` sets the new values, their timestamp and `Ready` in one step (also when recovering from
  `Degraded`/`Error`). No separate `StateChanged(Ready)` event exists, so a consumer can never see `Ready` together
  with an old value, and one successful cycle costs one queue slot.
* After a successful init a sensor is `WarmingUp` (SCD4x, SDS011) or still `Initializing` (BMx280, noise) until its
  first measurement arrives.
* Absence is not an error: a sensor that never answered is `NotDetected` and probed every 10 s.
* A sensor that was detected before and stops answering is `Error` and recovered with exponential backoff
  (1 s … 30 s), so a reconnected sensor returns to `Ready` without a reboot.
* First failure → `Degraded`; 3 consecutive failures → `Error` and driver re-initialization.
* Errors never erase the last valid observation; they are published as `SensorEvent::Error`.

### Timing semantics

Four delays exist and are never added up:

| Delay | Owner | Meaning |
|-------|-------|---------|
| Initial delay | driver (`initial_delay_ms`, default 0) | Wait before the first sample after first detection. No sensor needs one: the SDS011 warms up inside its own cycle |
| Poll interval | driver (`poll_interval_ms`) | Wait **after a successful** sample before the next one |
| Recovery backoff | runner (`RunnerConfig`) | Wait **after a failed** sample or init: 1 s, 2 s, 4 s … capped at 30 s; reset by the next successful *measurement* |
| Warm-up | driver, inside `sample` | Hardware-specific (SDS011 fan spin-up, SCD4x first reading) |

```text
success:  sample -> publish -> poll interval -> sample
failure:  sample -> error   -> backoff       -> sample   (no poll interval on top)
```

Every sensor runs in its own Embassy task, so a failing or slow sensor never delays another one.

## Event queue

`EventBus` is a bounded queue of 16 events (`EVENT_QUEUE_CAPACITY`). When it is full, the **oldest** event is dropped
and counted (`EVENTS.dropped()`, logged with every snapshot): newest state is more useful than a backlog and producers
never block. The store task is the only component mutating the `MeasurementStore`.

## Timing

| Sensor | Cadence | Details |
|--------|---------|---------|
| BMP280 / BME280 | 10 s | Forced mode: T ×2, P ×4, H ×1, filter off, ≈19 ms per conversion, I²C `0x76` or `0x77` (auto-probed, see below) |
| SCD4x | 5 s | Periodic measurement, data-ready polling every 500 ms (max 20 polls), CRC-8 on every word |
| SDS011 | ≈125–150 s cycle | Query mode: 30 s warm-up, 5 valid samples 1.5 s apart (window ≤ 30 s), aggregate, 90 s sleep; see below |
| Noise | 10 s window | 16 kHz mono; see below |

All waits use monotonic async timers; nothing depends on wall-clock time. Every I²C transaction is bounded to 200 ms
(`TimeoutI2c`), every UART/I²S read to SDS011 5 s / 1 s (I²S).

### BMP280 / BME280 address

The driver probes `0x76` first and then `0x77` and uses the first address whose chip ID register reads a BMP280
(`0x56..=0x58`) or BME280 (`0x60`); other IDs are skipped. If both addresses hold a valid sensor, `0x76` wins (two
BMx280 are not supported). Probing is repeated on every re-init, so a sensor moved to the other address is found
without a reboot. If nothing is found the failure is reported as bus error, unsupported chip ID or no device (in that
order of precedence); the last two keep the sensor `NotDetected`.

### SDS011 cycle

The command flow and wire protocol (`sensors/sds011/protocol.rs`) are derived from the community
[`sds011-rs`](https://github.com/kenohassler/sds011-rs) driver (MIT OR Apache-2.0). The sensor is kept in query mode and
asleep between cycles:

```text
wake -> warm-up (30 s) -> sample 1..N -> filter/aggregate -> publish PM2.5 / PM10 -> sleep (90 s)
```

* Sampling constants (`sensors/sds011/mod.rs`): `WARMUP_MS` 30 s, `SAMPLE_COUNT` 5, `SAMPLE_INTERVAL_MS` 1.5 s,
  `CYCLE_TIMEOUT_MS` 30 s (sampling window), `TRIM_MIN_MAX` on, `SLEEP_MS` 90 s. The values are a starting point to be
  confirmed on hardware against the legacy firmware behaviour.
* Only valid samples count: malformed, truncated or checksum-invalid frames and values above 999.9 µg/m³ are dropped.
  Sampling stops when `SAMPLE_COUNT` valid samples exist or the window is over.
* Aggregation (`sds011/aggregate.rs`, fixed arrays, no heap): rounded integer mean per channel; with at least 5
  samples the lowest and highest sample are discarded first, independently for PM2.5 and PM10 (e.g. PM2.5
  `10 12 11 80 9` → drop `9` and `80` → `11`).
* Too few valid samples → `Error::InsufficientSamples` (`SensorEvent::Error`, `Degraded` → `Error`, recovery backoff);
  a partial result is never published. A silent sensor fails the cycle at once with `NoResponse`.
* On top of that the driver bounds every UART read to 5 s, discards stale bytes (e.g. frames of a sensor still in
  active mode), skips unexpected frames, retries the wake-up up to 3 times and puts the sensor to sleep after every
  failed session so the fan does not keep running.
* The first measurement starts right after boot: the cycle's own warm-up is the only wait.

### Noise

Metric: **sound pressure level in dB SPL (re 20 µPa), unweighted**. It is *not* dBA and no A-weighting is applied.

| Property | Value |
|----------|-------|
| Microphone | ICS-43434 assumed: −26 dBFS at 94 dB SPL (1 kHz), so 24-bit full scale = 120 dB SPL |
| Sample rate | 16 kHz mono, 24-bit samples |
| Filtering | ≈10 Hz integer DC blocker only; otherwise flat |
| Sub-window | 125 ms mean square → one level |
| Measurement window | 10 s (80 sub-windows); *average* = energy average (`Leq`), *maximum* = loudest sub-window |
| Calibration | factory sensitivity only (`CALIBRATION_OFFSET_DB`), no per-device calibration |

Levels include the microphone's frequency response and the device's own noise (e.g. the SDS011 fan); they compare
between devices of the same microphone model, not with a calibrated A-weighted sound level meter. This is the v1
semantics the Connectivity Protocol schema can rely on; an A-weighted level would be added as a separate, tested
metric under its own name.

Raw audio is never stored: a 256-sample stack chunk plus the 8 KiB DMA ring buffer (≈128 ms) are the only buffers.

### Freshness

Measurements become `Stale` when older than the sensor's `FreshnessPolicy` (defaults in `rohi-sensor-core`). A host test
in `rohi-hal` asserts that every default exceeds the worst healthy cadence by at least 1.5×:

| Sensor | Worst healthy cadence | Stale after |
|--------|-----------------------|-------------|
| BMx280 | 10 s | 60 s |
| SCD4x | 15 s | 30 s |
| SDS011 | 150 s (warm-up + sampling window + sleep) | 300 s |
| Noise | 10 s | 30 s |

### SDS011 and noise coordination

The legacy firmware coordinated SDS011 activity with noise measurement (the fan is a noise source). This could not be
verified on current hardware here, so no coordination is implemented. The runtime exposes `STATES`
(`SensorStates`), a read-only view of every sensor state, so a future noise acquisition policy can react to the
SDS011 state (e.g. flag windows while it is in `Warmup`) without coupling the drivers.

## Pins

All GPIO assignments live in `board/altruist_c3.rs` and are verified for the Altruist Urban C3 (see the file header for
the source); drivers and firmware tasks never name pins:

| Function | Pin |
|----------|-----|
| I²C0 SDA / SCL | GPIO3 / GPIO0 |
| SDS011 UART1 TX / RX | GPIO10 / GPIO1 |
| I²S BCLK / WS / DIN | GPIO7 / GPIO6 / GPIO8 |

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
