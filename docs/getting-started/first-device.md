# First device: Air Quality Terminal

The fastest way to learn ROHI is to run a real example on a board and then change it. This page runs one sensor
example, explains it, and shows how to work on it with an AI assistant. The commands assume the toolchain from
[Installation](installation.md) (`nix develop` if you use Nix); [Build and test](build-and-test.md) comes next and
covers the full set of checks.

## 1. Pick an example

The Altruist board has one example per supported sensor in
[`examples/src/bin`](https://github.com/akagi-dev/rohi-sdk/tree/master/examples/src/bin):

| Example                | Sensor                                   |
|------------------------|------------------------------------------|
| `example-bmx280`       | BMP280 / BME280 pressure, temperature, humidity |
| `example-scd4x`        | SCD40 / SCD41 CO₂, temperature, humidity |
| `example-sds011`       | SDS011 particulate matter (PM2.5 / PM10) |
| `example-noise`        | I²S microphone noise level               |
| `example-all-sensors`  | All of the above, concurrently           |
| `example-config`       | configuration persistence self-test      |

Check in the [support matrix](../support/matrix.md) that your board carries the sensor: the Altruist Urban has an
SDS011, a BME280 and a microphone, and **no SCD4x**. This page uses the
[SDS011 example](https://github.com/akagi-dev/rohi-sdk/blob/master/examples/src/bin/sds011.rs). One cycle takes about
two minutes: the sensor wakes up, warms up, takes several samples, and the aggregated PM2.5 / PM10 is printed before
the sensor goes back to sleep.

## 2. Flash and run it

Connect the board with USB and run it like any Rust binary:

```bash
cargo run --release -p rohi-examples --features esp32c3 --target riscv32imc-unknown-none-elf --bin example-sds011
# ESP32-C6: --features esp32c6 --target riscv32imac-unknown-none-elf
```

Behind the scenes:

1. the Rust compiler creates a RISC-V binary;
2. `espflash` detects the board and flashes it;
3. `espflash` opens the serial console and shows the log, where you should see the sensor state change and, after a
   cycle, a PM2.5 / PM10 measurement.

If nothing is printed, the sensor may not be wired as the pin map of your board revision says: see
[Boards](https://docs.rs/rohi-hal-esp). The ESP32-C6 pin map is not yet verified on hardware.

## 3. Read the example

```rust,ignore,noplayground
{{#include ../../examples/src/bin/sds011.rs:17:}}
```

- **no_std.** The file starts with `no_*` declarations, typical for bare-metal software without an operating system
  or standard library.
- **Main task.** `main` is a plain `#[esp_hal::main]` function. `run()` configures logging, the `esp-rtos` runtime that
  provides the time base of the [Embassy Framework](https://embassy.dev/) (with a small heap for its task stacks) and
  the board (I²C, UART and I²S pins are defined in `crates/hal-esp/src/board/altruist_urban/`), then starts the
  Embassy executor, which spawns the two tasks below. See the
  [Embassy Docs](https://embassy.dev/book/#_task_declaration).
- **Sensor task and events.** `sds011_task` runs the SDS011 driver in a `Runner` that handles probing, polling and
  recovery. Drivers know nothing about networks or serialization: they publish typed `SensorEvent`s (measurements,
  state changes, errors) into a bounded queue; a successful measurement alone marks the sensor `Ready`.
  `print_events_task` prints them. The production firmware has a single task that applies the events to a
  `MeasurementStore` instead.

<div class="warning">

Each `await` puts the CPU to sleep until a waiting event occurs: a timer interval, or a measurement from a peripheral.
Instead of a busy loop the CPU enters a low-power mode, and other tasks can use the time.

</div>

## 4. Work on it with an AI assistant

The examples are small enough for an AI coding assistant to understand in one pass, which makes them the best starting
point for a custom firmware. Open the repository in your assistant and try prompts like these, one at a time:

**Understand it**

```text
Read examples/src/bin/sds011.rs and the sds011 driver in crates/hal/src/sensors/sds011.
Explain how a measurement gets from the UART to the print_events_task, and which
parts are hardware independent. Do not change any file.
```

**Change it**

```text
In examples/src/bin/sds011.rs print the PM2.5 value together with a plain-language
air quality label (good / moderate / unhealthy). Keep the driver untouched.
Then run `cargo check -p rohi-examples --no-default-features --features esp32c6
--target riscv32imac-unknown-none-elf` and show me the result.
```

**Combine examples**

```text
Using examples/src/bin/bmx280.rs and sds011.rs as a model, create a new example
that reads the BME280 and the SDS011 concurrently and prints one line per
measurement. Follow the structure of examples/src/bin/all-sensors.rs.
```

**Check the claim**

```text
Look at docs/support/matrix.md (or support-matrix/matrix.toml). Is the sensor I am
using supported on the Altruist Urban ESP32-C6, and what is verified on hardware?
Answer only from the matrix and tell me what is missing.
```

Review what the assistant produces and run it on your board yourself: an assistant can make code compile, but only you
can verify that it works on the hardware. The full workflow for changing a firmware with an assistant, with more
prompts, is in [Customize firmware with AI](../build/customize-with-ai.md).

## 5. Talk to the device

The production firmware (`altruist-urban`, not the examples) advertises over Bluetooth LE, and the
[`rohi` desktop application](../use/desktop/index.md) shows its status, sensors and Wi-Fi.

## Next

[Build and test](build-and-test.md) runs the host tests and the ESP checks, and explains what each one proves.
