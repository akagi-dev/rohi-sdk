# Add a sensor

A sensor is a **driver** in `rohi-hal` managed by a generic `Runner`; the board crate supplies the bus, the firmware
spawns one task. Drivers know nothing about networks or serialization. Background: [Sensor HAL](../sdk/hal.md),
[Sensors](../sdk/sensors.md).

1. **Model the observation** (portable, `rohi-hal`). Add the sensor to `SensorKind` and give it a typed update in
   `MeasurementUpdate` (units from `units.rs`: `Celsius`, `Ppm`, ...). Measurements are grouped by the physical sensor;
   never merge readings of different sensors. Give the sensor a freshness policy
   (`FreshnessPolicy::default_for`) that outlasts its worst healthy cadence: a test in `sensors/mod.rs` enforces it.
2. **Write the driver** in `crates/rohi-hal/src/sensors/<name>.rs` and implement `SensorDriver`:
   `init` (probe and configure; called again to recover), `sample` (wait for and read the next observation,
   `Ok(None)` = no data yet), `poll_interval_ms`, optionally `initial_delay_ms`, plus `KIND` and `WARMS_UP`. Map the bus
   error into `DriverError` (`code`, and `is_absent` for "does not answer", which makes the state `NotDetected`
   instead of `Error`).
3. **Test on the host.** In the driver module, drive the driver with `MockI2c` / a fake UART from the crate-internal
   `testutil` (`crates/rohi-hal/src/testutil.rs`), and the runner with
   a recording delay. Cover detection, a failing bus, recovery, warm-up and the exact decoding of datasheet examples.
   See [Host testing](../validation/host-testing.md).
4. **Wire the bus** (platform). If the sensor needs pins or a new peripheral, add them to the board module in
   `rohi-hal-esp` (see [Add a board](add-board.md)); drivers never name pins.
5. **Spawn a task** in `firmwares/altruist-urban/src/tasks/sensors.rs`: build the driver, wrap it in
   `Runner::new(driver, reporter(SensorKind::X), Delay, RunnerConfig::DEFAULT)` and `runner.run().await`; start it in
   `main.rs` with the other sensor tasks. I²C sensors share one bus through `i2c_handle`.
6. **Expose it** to the control plane if it should be visible: add its measurements as status fields (stable ids, names
   `<sensor>.<measurement>`, canonical units) and its issue codes in `rohi-hal/src/sensors_service.rs`; the table in
   [Sensors](../sdk/sensors.md#status-and-the-control-plane) lists them. The protocol and the `rohi` tool need no change
   (see [Add a service](add-service.md)).
7. **Add a hardware example** in `examples/src/bin/` if useful, and record the validation in
   [Hardware testing](../validation/hardware-testing.md).
8. **Update the docs** ([policy](../contributing/docs-style.md)): the sensor's page section in
   [Sensors](../sdk/sensors.md) and any new limit in [Resource limits](../reference/resource-limits.md).

Rules to respect: the runner owns the driver (one task per sensor); do not call `step` in a `select` unless you accept
that a bus transaction may be cut short ([cancellation](../concepts/async-and-cancellation.md)); do not allocate.
