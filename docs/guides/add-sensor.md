# Add a sensor

A sensor is a **driver** in `rohi-hal` run by a generic runner; the board crate supplies the bus and the firmware spawns
one task. Drivers know nothing about networks or serialization. The contracts (driver trait, runner, observations,
freshness) are in the [`rohi-hal` rustdoc](https://docs.rs/rohi-hal) and the
[sensor drivers](https://docs.rs/rohi-hal/latest/rohi_hal/sensors/index.html).

1. **Model the observation** in `rohi-hal`: add the sensor kind and its typed measurement with its units, grouped by the
   physical sensor (never merge readings of different sensors), and give it a freshness policy that outlasts its worst
   healthy cadence.
2. **Write the driver** in `crates/hal/src/sensors/<name>.rs` against the driver trait: probe and configure, read the
   next observation, report an absent sensor differently from a failing one.
3. **Test on the host** with the mock bus and the recording delay: detection, a failing bus, recovery, warm-up and the
   exact decoding of datasheet examples. See [Host testing](../validation/host-testing.md).
4. **Wire the bus** in the board module of `rohi-hal-esp` if the sensor needs pins or a new peripheral (see
   [Add a board](add-board.md)); drivers never name pins.
5. **Spawn a task** in `firmwares/altruist-urban/src/tasks/sensors.rs` and start it in `main.rs` with the other sensor
   tasks.
6. **Expose it** to the control plane if it should be visible: add its measurements and issue codes to the `sensors`
   service (`crates/hal/src/sensors_service.rs`). The protocol and the `rohi` tool need no change; see
   [Add a service](add-service.md).
7. **Add a hardware example** in `examples/src/bin/` if useful and record the validation in
   [Hardware testing](../validation/hardware-testing.md). Update the
   [support matrix](../support/overview.md#maintenance-policy) (implementation, integration, evidence) without marking
   anything verified that was not run on hardware.
8. **Update the docs** ([policy](../contributing/docs-style.md)): the rustdoc of the driver and, for a new limit, the
   [memory model](https://docs.rs/rohi-sdk/latest/rohi_sdk/memory/index.html).

One task owns one driver, and a bus transaction must not be cut short by a `select`; see the
[cancellation rules](https://docs.rs/rohi-sdk/latest/rohi_sdk/cancellation/index.html). Do not allocate.
