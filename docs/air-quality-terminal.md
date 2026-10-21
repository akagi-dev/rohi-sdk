# Air Quality Terminal

> This section assumes you are using the Nix package manager (`nix develop`).
> If you have a different environment, don't worry—most commands will be the same,
> except you won't need to enter the Nix shell first.

If you have successfully installed the SDK, let's understand how the Altruist examples work step by step.
The Altruist board has one example per supported sensor:

| Example                | Sensor                                   |
|------------------------|------------------------------------------|
| `example-bmx280`       | BMP280 / BME280 pressure, temperature, humidity |
| `example-scd4x`        | SCD40 / SCD41 CO₂, temperature, humidity |
| `example-sds011`       | SDS011 particulate matter (PM2.5 / PM10) |
| `example-noise`        | I²S microphone noise level               |
| `example-all-sensors`  | All of the above, concurrently           |

We'll start from the [sds011.rs](https://github.com/akagi-dev/rohi-sdk/blob/master/examples/src/bin/sds011.rs)
example, it's based on a particulate matter sensor. One SDS011 cycle takes about two minutes: the sensor wakes up, warms
up, takes several samples, and the aggregated PM2.5 / PM10 is printed before the sensor goes back to sleep.

## Explore the code

```rust,no_run,noplayground
{{#include ../examples/src/bin/sds011.rs:17:}}
```

### no_std

The file starts with `no_*` declarations, which is typical for bare-metal software since there's no operating system
or standard library available by default.

### Main task

`main` is a plain `#[esp_hal::main]` function. `run()` configures logging, the time driver of the
[Embassy Framework](https://embassy.dev/) (no RTOS, no heap) and the board (I²C, UART and I²S pins are defined in
`crates/rohi-hal/src/board/altruist_c3.rs`), then starts the Embassy executor, which spawns the two tasks below.
See the [Embassy Docs](https://embassy.dev/book/#_task_declaration) for details.

### Sensor task and events

`sds011_task` runs the SDS011 driver in a `Runner` that handles probing, polling and recovery. Drivers know nothing
about networks or serialization: they publish typed `SensorEvent`s (measurements, state changes, errors) into a
bounded queue; a successful measurement alone marks the sensor `Ready`. `print_events_task` consumes them and
prints to the terminal. The production firmware has a single task that applies the events to a `MeasurementStore`
instead. See [Sensor Core](sensor-core.md).

<div class="warning">

You'll notice multiple `await` calls. This instruction puts the CPU to sleep until a waiting event occurs—whether it's
a time interval for a timer or reading measurements from a peripheral device. Rather than running in an infinite loop
checking conditions, the CPU enters a low-power mode to conserve energy and resources. During `await` periods, other
tasks can use the CPU time to execute their operations.

</div>

## Flashing

Flashing firmware is quite simple—just launch it using cargo as you would run any Rust binary.

```bash
cargo run --release --bin example-sds011
```

Behind the scenes, several processes occur:

1. The Rust compiler creates a RISC-V binary;
2. `espflash` detects the ESP board and flashes the firmware;
3. `espflash` connects to the serial interface and displays logs.

<script src="https://asciinema.org/a/TD4TphAuoOyL7njcRor7Vg0dj.js" id="asciicast-TD4TphAuoOyL7njcRor7Vg0dj" async="true"></script>
