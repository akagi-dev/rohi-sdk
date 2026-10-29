# Sensor HAL (`rohi-hal`)

The Altruist Urban firmware turns physical sensors into immutable `MeasurementsSnapshot` values:

```text
physical sensor -> rohi-hal driver -> Runner (one Embassy task per sensor) -> SensorEvent
  -> bounded event queue -> store task -> MeasurementStore -> MeasurementsSnapshot
```

Dependency direction: `altruist-urban` → `rohi-hal-esp` → `rohi-hal`. `rohi-hal` holds the typed measurements, the
store, the runtime and the drivers, and is platform independent. Drivers know nothing about Wi-Fi, BLE,
serialization or any other transport, and nothing is averaged or fused across sensors: `bmx280.temperature` and
`scd4x.temperature` are separate observations. Per-sensor details are in [Sensors](sensors.md), pins in [Boards](../platform/boards.md).

## Runtime

The firmware runs Embassy on top of `esp-rtos` (thread-mode executor and the 1 MHz `embassy-time` driver),
which the Wi-Fi radio driver requires. The only heap is the fixed radio heap owned by the ESP wireless runtime ([Wireless runtime](../platform/wireless.md)); sensor
and application code is heapless. All waits use monotonic async timers; nothing depends on wall-clock time.

## Components

| Component | Location | Role |
|-----------|----------|------|
| Drivers | `crates/hal/src/sensors/` | BMx280, SCD4x, SDS011, noise. Hardware independent (`embedded-hal-async`, `embedded-io-async`), unit tested on a host |
| Runtime | `crates/hal/src/runtime/` | `Runner` lifecycle, `Reporter`, `EventBus`, backoff, bounded I²C |
| Types, store | `crates/hal/src/` (`measurements`, `sensor`, `store`, ...) | Typed units, `SensorEvent`, `MeasurementStore`, `MeasurementsSnapshot` |
| Board | `crates/hal-esp/src/board/altruist_urban/` | All pin assignments, bus configuration (features `esp32c3` / `esp32c6`) |
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

Other tasks never see the store. They call `tasks::store::handle().snapshot().await` (a `MeasurementHandle`, from
`rohi_hal::runtime::service`): a bounded request/response to the store task, which generates the snapshot at request
time with the current monotonic clock, so `Valid`/`Stale` are correct when asked. `try_snapshot()` returns `Busy`
instead of waiting for another request in flight.
## Freshness

A valid measurement becomes `Stale` when it is older than the sensor's `FreshnessPolicy` (defaults in
`rohi-hal`, tunable with `MeasurementStore::set_freshness_policy`). A host test in `rohi-hal` asserts that every
default exceeds the worst healthy cadence of its sensor by at least 1.5×:

| Sensor | Worst healthy cadence | Stale after |
|--------|-----------------------|-------------|
| BMx280 | 10 s | 60 s |
| SCD4x | 15 s | 30 s |
| SDS011 | 150 s (warm-up + sampling window + sleep) | 300 s |
| Noise | 5 s | 30 s |

Errors do not make a value stale; they mark it `Error` while the last valid value is preserved.

## SDS011 and noise coordination

The legacy firmware coordinated SDS011 activity with noise measurement (the fan is a noise source). This is not
implemented. The runtime exposes `STATES` (`SensorStates`), a read-only view of every sensor state, so a future noise
policy can react to the SDS011 state (e.g. flag windows while it is in `WarmingUp`) without coupling the drivers.

## Testing and resources

The runner timing and recovery, event consistency, the store, SDS011 sampling and aggregation, BMx280 address probing
and the freshness thresholds are covered by host tests ([Host testing](../validation/host-testing.md)). Hardware
examples, the combined sensor checklist and the stability run are in [Hardware testing](../validation/hardware-testing.md);
flash and RAM measurements are in [Resource budget](../validation/resource-budget.md#sensor-runtime-rohi-hal).

## Cancellation

The sensor runner is meant to run to completion: `Runner::step` may be dropped (for example at shutdown) but a sensor
transaction in flight is then cut short, and the next step repeats the interrupted attempt. The warm-up wait is not lost
when a step is dropped. `EventBus::receive` and the owner side `MeasurementService::accept` are cancel-safe;
`MeasurementHandle::snapshot` can be dropped at any point. Details: [Async and cancellation](../concepts/async-and-cancellation.md).
