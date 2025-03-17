# Air Quality Terminal

> This section assumes you are using the Nix package manager. 
> If you have a different environment, don't worry—most commands will be the same, 
> except you won't need to launch nix-shell first.


If you have successfully built the example, let's understand how it works step by step.

The Altruist board currently has a set of examples for different sensors:

- `altruist-sensor-temp.rs`
- `altruist-sensor-press.rs`
- `altruist-sensor-pm25.rs`

We'll start from [altruist-sensor-pm25.rs](https://github.com/akagi-dev/rohi-sdk/blob/master/examples/src/bin/altruist-sensor-pm25.rs) example, it's based on PM sensors.

## Explore the code

### no_std

```rust,no_run,noplayground
{{#include ../examples/src/bin/altruist-sensor-pm25.rs:18:20}}
```

The file starts with `no_*` declarations, which is typical for bare-metal software since there's no operating system or standard library available by default.

### Imports

```rust
{{#include ../examples/src/bin/altruist-sensor-pm25.rs:21:28}}

```

Here we import [Embassy Framework](https://embassy.dev/) related types and SDK types like `Altruist` and `Sensor`.

### Main task

```rust
{{#include ../examples/src/bin/altruist-sensor-pm25.rs:40:47}}

```

The main task spawns first and handles basic initialization and child task launching. See the [Embassy Docs](https://embassy.dev/book/#_task_declaration) for details.

Here, the `Altruist` hardware is initialized and stored in a local variable, then passed to the child working task `print_pm25_task`.

### Work task

```rust
{{#include ../examples/src/bin/altruist-sensor-pm25.rs:30:38}}

```

The work task handles all operations—in this case, it simply gets PM2.5 measurements and prints them to the logs (terminal).

<div class="warning">

You'll notice multiple `await` calls. This instruction puts the CPU to sleep until a waiting event occurs—whether it's a time interval 
for `Timer` or reading measurements from a peripheral device for `sensor`. Rather than running in an infinite loop checking conditions, 
the CPU enters a low-power mode to conserve energy and resources. During `await` periods, other tasks can use the CPU time to execute their operations.

</div>

## Flashing

Flashing firmware is quite simple—just launch it using cargo as you would run any Rust binary.

```bash
cargo run --release --bin example-altruist-sensor-pm25
```

Behind the scenes, several processes occur:

1. The Rust compiler creates a RISC-V binary;
2. `espflash` detects the ESP board and flashes the firmware;
3. `espflash` connects to the serial interface and displays logs.

<script src="https://asciinema.org/a/TD4TphAuoOyL7njcRor7Vg0dj.js" id="asciicast-TD4TphAuoOyL7njcRor7Vg0dj" async="true"></script>
