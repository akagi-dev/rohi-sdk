# Resource budget (measurements)

Measured sizes of the firmware and the libraries, per hardware or build run. The **limits** themselves (queue depths,
buffer sizes, heap size) are defined and explained in [Resource limits](../reference/resource-limits.md); this page only
records what was measured with them. Numbers here are history, not contracts: they change with every dependency update
and are re-measured when the limits change.

## Altruist Urban firmware (Wi-Fi + BLE control)

Measured with `size` on the release build (`cargo build-esp32c3` / `cargo build-esp32c6`), baseline = Wi-Fi only,
before the BLE control plane was added:

| | ESP32-C3 before | ESP32-C3 after | ESP32-C6 before | ESP32-C6 after |
|---|---:|---:|---:|---:|
| `.text` (flash) | 970 092 | 1 149 952 | 1 012 322 | 1 335 452 |
| `.data` | 10 024 | 15 640 | 12 876 | 18 396 |
| `.bss` | 106 984 | 181 248 | 111 288 | 185 056 |
| wireless heap (in `.bss`) | 73 728 | 131 072 | 73 728 | 131 072 |
| `.bss` without heap | 33 256 | 50 176 | 37 560 | 53 984 |

Statically allocated control buffers: reassembly 256 B + response 2 048 B + chunk 244 B (control task), BLE RX/TX
queues 4 × ~250 B each, TrouBLE host resources, and the task futures. The application code stays heapless; only
the radio drivers use the wireless heap.


## Sensor runtime (`rohi-hal`)

`cargo build-esp32c3`, ESP32-C3 (before Wi-Fi/BLE/TLS):

| Item | Size |
|------|------|
| `.text` (flash) | 114 370 B |
| `.rodata` (flash) | 24 096 B |
| `.rwtext` + `.data` (RAM) | 1 700 B + 2 132 B |
| `.bss` (RAM, includes task futures) | 9 160 B |
| Heap | fixed radio heap only (see [Memory model](../concepts/memory-model.md)); application code does not allocate |
| Event queue | 16 events |
| Noise buffers | 256 samples (1 KiB) stack + 8 KiB DMA ring |

Task stack usage is not measured yet.

### Stack and the control plane

All embassy tasks are polled on the one main stack, which on the ESP32-C3 build of `altruist-urban` is the linker `.stack`
section: about 42 KB (`_stack_end` to `_stack_start` of the release ELF) and it sits directly above `.bss`, so an
overflow corrupts task state (observed as a `Load access fault` in the embassy timer queue with the ASCII of a service
name in the registers). The control task builds and moves protobuf responses on that stack, so response sizes are
bounded: the largest `Request` or `Response` variant is kept under about 2.7 KB on the host
(`messages_stay_small_in_memory` in `rohi-control-proto`), and long schemas, status values and action lists are paged
instead of grown. The static future of `control_task` is about 11 KB of `.bss` (release build, ESP32-C3, measured with
`nm -S` on `altruist_urban::control::control_task::POOL`). Stack depth under load is not measured yet. The stack is what the linker leaves after `.bss`, so every
static byte and every deep frame on the config or control path costs stack: a decoder that builds a large section from
temporaries (the `network` section, about 200 B) needed ~900 B of frame and overflowed the stack on a `rohi config` request;
`NetworkConfig` is decoded in place for that reason. Check `_stack_end` and the frames of the config and control path
when a section or a static grows.


## Configuration store

The store uses no heap; see [Resource limits](../reference/resource-limits.md#configuration-store) for its bounds. Its
RAM cost is a few words of state plus the stack workspace during a call.

## IP stack, TCP and UDP pools

Not measured: no SDK component opens TCP connections yet, and the UDP pool (one socket, 2 x 128 B plus packet metadata)
used by the NTP time source has not been measured on hardware either; the `ntp_task` stack and the time service state
are small and also *not measured*. Measure the `.bss` contribution of `NetResources` and the
minimum free wireless heap with an open connection when the first uplink component lands, and record it here.

## How to measure

```bash
cargo build-esp32c3 && size target/riscv32imc-unknown-none-elf/release/altruist-urban
cargo build-esp32c6 && size target/riscv32imac-unknown-none-elf/release/altruist-urban
```

The minimum free wireless heap comes from `rohi_net_esp::wireless_heap()` (the firmware logs it with its snapshot; the
`heap-stats` feature adds the minimum).
