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
bounded by the compact protocol layout and by explicit budgets, see [Control protocol types](#control-protocol-types).
The static future of `control_task` is about 11.6 KB of `.bss` (11 856 B, release build, ESP32-C3, measured with
`nm -S` on `altruist_urban::control::control_task::POOL`). **Stack depth under load is not measured yet**: it needs a
device, see [the plan to measure it](#measuring-the-control-task). The stack is what the linker leaves after `.bss`, so every
static byte and every deep frame on the config or control path costs stack: a decoder that builds a large section from
temporaries (the `wifi` section, about 200 B) needed ~900 B of frame and overflowed the stack on a `rohi config` request;
`WifiConfig` is decoded in place for that reason. Check `_stack_end` and the frames of the config and control path
when a section or a static grows.


### Control protocol types

The generated types of the control protocol keep every bounded field inline, so their size is the size of their largest
possible content. `generated_types_stay_within_their_memory_budgets` (`rohi-control-proto`, runs in CI) asserts a budget
for each; the sizes are `size_of` on the host (64-bit), before and after the compact layout of the protocol (numeric
ids and enum values, `f32`, compact `FieldDescriptor`, enums and issue names in the schema, no descriptions, no
summary in the detailed status, no pagination):

| Type | Before | After | Budget |
|------|-------:|------:|-------:|
| `Request` | 760 | 736 | 768 |
| `Response` | 2 616 | 1 824 | 1 900 |
| `FieldDescriptor` | 352 | 48 | 56 |
| `FieldValue` | 88 | 56 | 64 |
| `GetServicesStatusResponse` | 792 | 536 | 576 |
| `GetServiceStatusResponse` | 1 272 (8 values) | 1 192 (20 values) | 1 280 |

The `Response` is the largest of its variants (the service schema). The before column paged status values by eight, schemas by
four; the after column holds a whole service status (20 values), a whole schema (20 fields, an enum table, 12 issue
names) and 12 actions in one response. Raising a budget is a deliberate decision: measure the control task again and
update this table.

### Measuring the control task

The stack high-water mark of the control path has to be measured on a device, so that a future change has a concrete
baseline to be compared with. **Not measured yet**: the table below is the record to fill in. What is known without a
measurement: `rohi status` completes with Wi-Fi configured, an identity provisioned and all services active (reported
on the pull request that introduced the generic services); the previous stack overflow is not reproducible. How much
headroom remains is not known.

**Procedure.** Build the release profile of `altruist-urban` for the target (`cargo build --release -p altruist-urban
--no-default-features --features esp32c3 --target riscv32imc-unknown-none-elf`, `esp32c6` with
`riscv32imac-unknown-none-elf`) and read the stack size from the ELF (`_stack_start - _stack_end`, e.g. with
`riscv32-esp-elf-nm` or `rust-nm`). Paint the stack with a known pattern at boot (fill the region between `_stack_end`
and the current stack pointer, keeping a guard word at the bottom), run one scenario at a time from the table, then read
the stack with a debugger or dump it from a debug-only command and find the lowest address whose pattern changed. Use
the same firmware build for every row and reboot between scenarios. Maximum usage is `_stack_start - lowest touched
address`; headroom is the stack size minus that.

Scenarios, each with the device in the stated state (all services active, i.e. the Wi-Fi and BLE services running):

1. unconfigured device (no Wi-Fi, no identity)
2. Wi-Fi configured
3. identity provisioned
4. Wi-Fi and identity both configured: `rohi status` (**the regression scenario**) and `rohi status <service>` for
   every service
5. `rohi config get` and `rohi config set` of every section
6. Wi-Fi scan and `wifi connect` action invocation
7. identity `provision` and `erase` action invocation
8. `rohi factory-reset`
9. a reboot request

Record per target:

| Field | ESP32-C3 | ESP32-C6 |
|-------|----------|----------|
| Hardware (board, chip revision) | | |
| Firmware build (commit, profile) | | |
| Stack size (bytes) | | |
| Static `control_task` future (bytes, `nm -S`) | | |

| Scenario | ESP32-C3 max usage / headroom | ESP32-C6 max usage / headroom |
|----------|-------------------------------|-------------------------------|
| 1. unconfigured | | |
| 2. Wi-Fi configured | | |
| 3. identity provisioned | | |
| 4. `rohi status`, Wi-Fi + identity | | |
| 4. `rohi status <service>`, worst service | | |
| 5. `config get` / `config set`, worst section | | |
| 6. Wi-Fi scan / connect | | |
| 7. identity provision / erase | | |
| 8. factory reset | | |
| 9. reboot request | | |

**Safety margin (proposed).** Until measured, no margin is claimed. When the table is filled in, state the lowest headroom of any
row and keep at least 25 % of the stack (or 4 KB, whichever is larger) free in the worst row; a change that cuts into it
needs another measurement. The generated-type budgets in [Control protocol types](#control-protocol-types) stay in CI
independently of this measurement.

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
