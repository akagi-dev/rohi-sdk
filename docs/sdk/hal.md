# Sensor HAL (`rohi-hal`)

`rohi-hal` turns physical sensors into immutable `MeasurementsSnapshot` values: "the latest trustworthy reading of every
sensor, with its age and health", independent of the bus a sensor is on and of the transport that reports it. It holds
the typed units and measurements, the `MeasurementStore`, the sensor runtime (a lifecycle `Runner` per sensor, a bounded
event queue, on-demand snapshots) and the sensor drivers, described in [Sensors](sensors.md). The crate is `no_std`,
allocation free and names no chip, so the whole crate, drivers included, is tested on a host.

```text
physical sensor -> driver -> Runner (one task per sensor) -> SensorEvent -> bounded EventBus
  -> store task -> MeasurementStore -> MeasurementsSnapshot -> consumers
```

Measurements are observations, not interpretation: nothing is averaged or fused across sensors, so `bmx280.temperature`
and `scd4x.temperature` stay separate. A failing or slow sensor never delays another one, an error never erases the last
valid value, and a value older than its freshness policy becomes `Stale`. All waits use monotonic async timers; nothing
depends on wall-clock time ([Async and cancellation](../concepts/async-and-cancellation.md),
[Runtime ownership](../concepts/runtime-ownership.md)).

`rohi-hal-esp` is the platform side: it adapts `esp-hal` peripherals to the traits the drivers use and owns the board
support, that is which GPIO is which bus, bus configuration and the monotonic clock, for ESP32-C3 and ESP32-C6. It is
empty without a chip feature ([Targets and features](../reference/targets-and-features.md)). The dependency direction is
firmware (`altruist-urban`) to `rohi-hal-esp` to `rohi-hal`; pins are listed in [Boards](../platform/boards.md) and a new
board or sensor is added as described in [Add a board](../guides/add-board.md) and [Add a sensor](../guides/add-sensor.md).

| Item | Role |
|------|------|
| `MeasurementStore`, `MeasurementsSnapshot`, `Measurement` | Single-owner store of the latest observations and the immutable snapshot with freshness resolved at request time |
| `SensorKind`, `SensorState`, `SensorEvent` | Sensor identity, lifecycle state and the events sensors publish |
| `runtime::Runner`, `SensorDriver`, `EventBus`, `MeasurementHandle` | Lifecycle, timing and recovery, the bounded queue, and snapshot access for other tasks |
| `SensorsService` | The `sensors` service of the management plane |
| `sensors::{bmx280, scd4x, sds011, noise}` | Drivers |
| `rohi-hal-esp` board modules | Pins, buses, audio and clock of a concrete board |

Status: implemented and host-tested; the firmware builds in CI for ESP32-C3 and ESP32-C6. No sensor is recorded as
hardware-verified ([Support matrix](../support/overview.md), [Hardware testing](../validation/hardware-testing.md)).
SDS011 activity is not coordinated with noise measurement. Flash and RAM measurements are in
[Resource budget](../validation/resource-budget.md#sensor-runtime-rohi-hal).

## Reference

The crate rustdoc is the authoritative technical documentation (lifecycle, timing semantics, events, freshness,
cancellation):

- `rohi-hal`: [docs.rs](https://docs.rs/rohi-hal), [source](https://github.com/akagi-dev/rohi-sdk/tree/master/crates/hal)
- `rohi-hal-esp`: [docs.rs](https://docs.rs/rohi-hal-esp), [source](https://github.com/akagi-dev/rohi-sdk/tree/master/crates/hal-esp)
- SDK-level concepts: [docs.rs/rohi-sdk](https://docs.rs/rohi-sdk)
- Locally: `cargo doc --no-deps --open -p rohi-hal` (the ESP crate needs a chip feature and target, see its docs)
