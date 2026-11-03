# Sensors

The sensor drivers live in `rohi-hal` (`crates/hal/src/sensors/`), not in a separate crate. They are hardware
independent, publish typed `rohi-hal` observations and are run by the generic lifecycle runner described in
[Sensor HAL](hal.md); the pins and buses of a board come from `rohi-hal-esp`. The Altruist Urban firmware (ESP32-C3 and
ESP32-C6) runs all four drivers. Which sensors are present on which board and what is verified is in the
[support matrix](../support/overview.md); no sensor is hardware-verified yet.

| Sensor | Driver | Bus | Measures |
|--------|--------|-----|----------|
| BMP280 / BME280 | `sensors::bmx280` | I²C `0x76` / `0x77` | Temperature, pressure, humidity (BME280 only) |
| SCD40 / SCD41 | `sensors::scd4x` | I²C `0x62` | CO₂, temperature, humidity |
| SDS011 | `sensors::sds011` | UART, 9600 baud | PM2.5, PM10, fan off between duty cycles |
| I²S microphone | `sensors::noise` | I²S | Noise level, unweighted dB SPL (not dBA) |

The model, store and status service also name the ZMOD4510, AGS3871 and RadSens sensors, but there is **no driver** for
them in this crate. Driver details (probing, sampling, aggregation, noise metric) are in the rustdoc of each driver
module. The telemetry encoder omits sensors the wire schema has no field for ([Telemetry](connectivity.md)).

## Status and the control plane

At the control boundary the typed model becomes the generic `sensors` [service](service.md) (`SensorsService`): every
measurement of a present sensor is a status field named `<sensor>.<measurement>` in the canonical unit of its type,
for example `rohi status sensors` shows `bmx280.temperature`. Observations of one quantity from different sensors are
never merged. The service is `disabled` when no sensor is detected, and a sensor in `degraded` or `error` state is
reported as an issue. The field table, units and limits are in the rustdoc of `rohi_hal::sensors`.

## Reference

The crate rustdoc is the authoritative technical documentation:

- `rohi-hal` (drivers): [docs.rs](https://docs.rs/rohi-hal), [source](https://github.com/akagi-dev/rohi-sdk/tree/master/crates/hal/src/sensors)
- `rohi-hal-esp` (board support): [docs.rs](https://docs.rs/rohi-hal-esp), [source](https://github.com/akagi-dev/rohi-sdk/tree/master/crates/hal-esp)
- SDK-level concepts: [docs.rs/rohi-sdk](https://docs.rs/rohi-sdk)
- Locally: `cargo doc --no-deps --open -p rohi-hal`
- Adding a driver: [Add a sensor](../guides/add-sensor.md)
