# Resource budget (measurements)

Measured sizes of the firmware and the libraries, per hardware or build run. The **limits** themselves (queue depths,
buffer sizes, heap size) are defined and explained in [Resource limits](https://docs.rs/rohi-sdk/latest/rohi_sdk/memory/index.html); this page only
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
| Heap | fixed radio heap only (see [Memory model](https://docs.rs/rohi-sdk/latest/rohi_sdk/memory/index.html)); application code does not allocate |
| Event queue | 16 events |
| Noise buffers | 256 samples (1 KiB) stack + 8 KiB DMA ring |

Task stack usage is not measured yet.

### Stack and the control plane

All embassy tasks are polled on the one main stack, which on the ESP32-C3 build of `altruist-urban` is the linker `.stack`
section: 22 024 B on the `firmware`-profile build at `ca17cb6` (121 848 B on ESP32-C6; an earlier revision of this page recorded about 42 KB, and the stack is what the linker leaves after the statics, so it shrinks when they grow) and it sits directly above `.bss`, so an
overflow corrupts task state (observed as a `Load access fault` in the embassy timer queue with the ASCII of a service
name in the registers). The control task builds and moves protobuf responses on that stack, so response sizes are
bounded by the compact protocol layout and by explicit budgets, see [Control protocol types](#control-protocol-types).
The static future of `control_task` is 17 440 B of `.bss` (`firmware` profile, ESP32-C3, from the size of `altruist_urban::esp::control::control_task::POOL`; the earlier 11 856 B were a release build of an older revision). [Task memory and stack budget](#task-memory-and-stack-budget-memcheck) reports every task pool, the stack and the frames in CI. **Stack depth under load is not measured yet**: it needs a
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
| `GetServicesStatusResponse` | 792 | 792 (12 services; 536 for 8) | 832 |
| `GetServiceStatusResponse` | 1 272 (8 values) | 1 192 (20 values) | 1 280 |
| `GetConfigSectionResponse` | n/a | 1 504 | 1 536 |
| `PatchConfigSectionRequest` | n/a | 728 | 768 |

The `Response` is the largest of its variants (the service schema). The before column paged status values by eight, schemas by
four; the after column holds a whole service status (20 values), a whole schema (20 fields, an enum table, 12 issue
names) and 12 actions in one response. `GetConfigSectionResponse` carries the effective values plus the compact list of overridden field ids (`overridden`), and `PatchConfigSectionRequest` its `set` / `clear` mutations; both are new rows, measured after the overrides model. Raising a budget is a deliberate decision: measure the control task again and
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
5. `rohi config <section>` and `rohi config <section> FIELD=VALUE` of every section
6. Wi-Fi scan and `wifi reconnect` action invocation
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
| 5. `config <section>` read / `FIELD=VALUE` patch, worst section | | |
| 6. Wi-Fi scan / connect | | |
| 7. identity provision / erase | | |
| 8. factory reset | | |
| 9. reboot request | | |

**Safety margin (proposed).** Until measured, no margin is claimed. When the table is filled in, state the lowest headroom of any
row and keep at least 25 % of the stack (or 4 KB, whichever is larger) free in the worst row; a change that cuts into it
needs another measurement. The generated-type budgets in [Control protocol types](#control-protocol-types) stay in CI
independently of this measurement.

## Task memory and stack budget (`memcheck`)

`scripts/memcheck.sh` builds the Altruist Urban firmware for a chip twice (the production image and an analysis image
with `-Z emit-stack-sizes`, same compiler and flags otherwise) and runs `memcheck` (`tools/memcheck`) on them. CI does
this for ESP32-C3 and ESP32-C6 (job `Memcheck`), publishes `task-memory-esp32c3.json`, `task-memory-esp32c6.json` and
`task-memory-report.md` (complete) and `task-memory-summary.md` (only what needs attention), and compares a pull request
with its base branch built from identical inputs. The pull request comment is the summary: one row of numbers per chip, then
failed or close-to-limit (90 %) budgets, errors and warnings, new, removed and grown tasks, changed sections and frame
growth, each list cut after eight entries. The complete report is the job summary of the run and the `task-memory-report`
artifact; the comment links to it.

```bash
scripts/memcheck.sh images esp32c3 out && scripts/memcheck.sh analyze esp32c3 out   # one chip
scripts/memcheck.sh report out                                      # task-memory-report.md and task-memory-summary.md
```

What each number is, and what it is not:

| Quantity | Source | Meaning |
|----------|--------|---------|
| Task pool bytes | size of the task's `POOL` static in the production ELF | RAM reserved for all instances of the task. A *static* value, exact. The number of instances is not in the symbol table and is reported as unknown |
| Main stack | linker symbols `_stack_start` / `_stack_end` | the stack all Embassy tasks are polled on. Allocation, not use |
| Frame | LLVM `.stack_sizes` of the analysis image | the stack of **one function**. It is not the stack depth of a task: call chains, inlining, dynamic dispatch, async suspension and interrupts are not analyzed. Functions without metadata (assembly, ROM and vendor blobs) have an unknown frame |
| Peak use, headroom | hardware observation only | `not measured` until a device run is attached; never inferred from the above |
| Native RTOS threads, interrupt contexts | not in the image | `not_detectable_statically` / `runtime_only`; a list of ELF symbols is not a list of run-time threads |

The budgets (`firmwares/altruist-urban/memcheck-budgets.toml`) are per chip: `.data`, `.bss`, the sum of the task pools,
a minimum for the main stack and a maximum single frame, plus the largest allowed growth against the base. A run fails
when a budget is exceeded, when the main stack is below its minimum, when the task inventory or the `.stack_sizes`
metadata is missing (an approved exception in the budget file is the only way around that), or when the base was built
with different inputs. Growth that stays below the limits is listed but allowed. The main stack is what the linker leaves
after the statics, so every static byte removed from `.bss` or `.data` is stack gained; a regression of the stack is a
budget failure.

**Baseline** (`master` at `ca17cb6`, ESP32-C3 `firmware` profile and ESP32-C6 `release` profile, Rust 1.99.1; all values
from the ELF, nothing measured on a device):

| | ESP32-C3 | ESP32-C6 |
|---|---:|---:|
| `.data` (with `.data.wifi`) | 29 792 | 33 084 |
| `.bss` (includes the 131 072 B wireless heap and the task pools) | 204 824 | 208 928 |
| Task pools, 15 tasks | 40 360 | 40 336 |
| `control_task` pool | 17 440 | 17 440 |
| `ble_task` pool | 7 064 | 7 040 |
| `connectivity_task` pool | 3 624 | 3 624 |
| `logging_task` pool | 2 304 | 2 304 |
| Main stack (`.stack`, placed directly above `.bss`) | 22 024 | 121 848 |
| Largest frame, `Controller` request handling | 30 112 | 29 104 |
| Frame of `Logging` read path | 8 400 | 4 432 |
| Frames with metadata | 1 520 of 3 827 (39.7 %) | 1 947 of 5 383 (36.1 %) |

The largest frame on both chips, the generic `Controller` request path, is larger than the whole ESP32-C3 main stack.
The analyzer reports this as a `frame-exceeds-stack` warning: a frame is not a peak, but a function with that frame
cannot run on a 22 KB stack, and it is the first candidate for a device investigation and for issue #63.

**Hardware observations.** To attach a measured stack peak, write a JSON file and pass it with `--hardware`
(`scripts/memcheck.sh analyze <chip> <out> --hardware observations.json`):

```json
{"observations": [{
  "chip": "esp32c3", "stack": "rtos/main", "scenario": "rohi status logging",
  "firmware_sha": "<commit of the flashed image>", "peak_used_bytes": 18200,
  "guard_state": "enabled", "notes": "board, revision, how the stack was painted"
}]}
```

Omit `peak_used_bytes` for a scenario that was not measured (`not measured` is not `0`). The report computes
`headroom = allocated - peak` and requires at least 25 % of the stack or 4 KiB, whichever is larger, unless the
observation sets a justified `required_margin_bytes`. Embassy tasks share the main stack, so there is no per-task stack
to measure; per-task attribution would need executor-aware instrumentation. The scenarios to record are the ones in
[Measuring the control task](#measuring-the-control-task) plus `rohi status logging`, `rohi logs clear --yes`, a log
flush with compression and BLE traffic. Without an observation the outcome is `NEEDS HARDWARE DATA`, never `PASS`. An observation whose `firmware_sha` differs from the analyzed image (`--git-sha`) is ignored with a `hardware-sha-mismatch` warning, so it cannot turn the outcome into `PASS`.

### Stack pressure of logging and the Control API (#63)

Measured with `memcheck` on the same toolchain, profile and flags, against `master` at `ca17cb6` (ESP32-C3 `firmware`
profile, ESP32-C6 `release` profile). Frames are the stack of one function: they show where a large frame went, not the
depth of any call chain. "At most N B" means the function left the 50 largest frames of the report, so N is the smallest
frame that remained in it (an upper bound, not a measurement). Nothing here was measured on a device.

| Change | Function | ESP32-C3 before → after | ESP32-C6 before → after |
|---|---|---:|---:|
| Streams are removed in place and only their segment ids are kept (`Logging::release_streams`, `close_stream`) | `Logging::release_streams` | 8 400 → at most 1 728 | 4 432 → at most 1 808 |
| | lock closure around `release_streams` | n/a | 6 304 → at most 1 808 |
| | `ActionRegistry::close_stream` | 2 352 → at most 1 728 | 2 352 → at most 1 808 |
| Every backend call is polled through `rohi_service::Isolated`; every request has its own function in `Controller` | `Controller::execute` poll | 30 112 → at most 1 728 | 29 104 → at most 1 808 |
| | `control_task` poll (inlines the request path; approved growth in the budget file) | 7 072 → 8 608 | 6 128 → 9 248 |
| | largest frame of the image | 30 112 → 12 240 (async main future, unchanged) | 29 104 → 9 248 (`control_task` poll) |
| Static RAM | `.data`, `.bss`, task pools, main stack | 0, −8, −8, +8 B | −32, 0, −8, +32 B |

What changed and why it works:

- `release_streams` collected the released streams *by value* into a second `Vec<Open<READ>, 2>` (two stream payloads
  of `READ` = 2 048 B plus bookkeeping). Streams are now removed in place with `retain_mut` and only the ids of the
  segments they pinned (at most 2 × 24 `u32`) are kept, then unpinned outside the stream table. `close_stream` takes the
  storage lock first and removes and unpins in one step, so a caller that gives up while waiting leaves the stream to
  expire with its pins instead of leaking them (before, the stream was removed first and the pins were released after the
  await). Regression tests: `clear_releases_one_and_two_open_streams`,
  `two_downloads_release_their_shared_segments_independently`,
  `an_expired_stream_is_released_while_the_other_keeps_its_pins`, `a_lost_link_releases_every_stream_at_once`,
  `closing_streams_in_any_order_leaves_the_other_one_intact`, `closing_a_stream_twice_is_an_error_and_releases_nothing_more`,
  `repeated_clear_and_logging_after_it_keeps_working`; each also checks the segments the *storage* still holds pinned.
- The 30 KB frame was not the response types: it was the poll of every config section, status and action of the device
  inlined into `Controller::execute`, each with its own copies of its bounded collections (`SectionValues`,
  `ServiceStatus`, `Records`, a few KiB each), which LLVM does not overlap. `rohi_service::Isolated` polls a future
  through a function that is never inlined, so only the call that runs has a frame. The registries of `rohi-service`
  poll each section, status and action through it, and `Controller` does the same for each request. Wrapping only
  `Controller` left a 19–22 KB frame in the status registry; wrapping the registries brought it below 1.8 KB.
- A first attempt, writing every response into one caller-provided slot, made the frame *larger* and the task future 1.5 KB
  larger (the slot lives across `await`s) and was dropped: the response types were not the cause.

Not done, with the reason:

| Item | Status |
|---|---|
| Status serialized without the `Collector` and the protobuf `Vec` at once; zero-result actions without a `RecordCollector` | not implemented: after the change above the frames of these paths are below the 1.8 KB reporting floor, and the `control_task` future (17 432 B) did not move; a smaller future needs the state of the largest await, which this analysis does not attribute |
| `RingStorage::scan_open` (2 056 B chunk + 2 048 B text) | not implemented: the `recover` poll frame (6 608 B on C3) is not on the control path; shrinking it needs a streaming LZ4 decoder, which changes recovery logic and needs a flash-damage test matrix |
| `Logging::collect()` / `SegmentDecoder` staging buffers | not implemented: a staging state machine needs a measured benefit, and the `collect` closure frame was 2 160 B on both chips, is at most 1 728 B on ESP32-C3 after this change (side effect of inlining) and unchanged on ESP32-C6 |
| LZ4 `HASH_BITS` 10 → 9 → 8 | not benchmarked: needs representative firmware logs (compression ratio, flash writes, segment rotation), which the repository does not contain |

Outstanding hardware verification: the control path (`rohi status`, `status logging`, `logs`, `logs download`,
`logs clear --yes`) on ESP32-C3 and ESP32-C6, with a main-stack high-water mark attached as a hardware observation; the
static result does not show that the exception seen on the device is fixed.

## Configuration store

The store uses no heap; see [Resource limits](https://docs.rs/rohi-sdk/latest/rohi_sdk/memory/index.html) for its bounds. Its
RAM cost is a few words of state plus the stack workspace during a call.

## IP stack, TCP and UDP pools

Not measured: no SDK component opens TCP connections yet, and the UDP pool (one socket, 2 x 128 B plus packet metadata)
used by the NTP time source has not been measured on hardware either; the `ntp_task` stack and the time service state
are small and also *not measured*. Measure the `.bss` contribution of `NetResources` and the
minimum free wireless heap with an open connection when the first uplink component lands, and record it here.

## How to measure

```bash
cargo build-esp32c3 && size target/riscv32imc-unknown-none-elf/firmware/altruist-urban-firmware
cargo build-esp32c6 && size target/riscv32imac-unknown-none-elf/release/altruist-urban-firmware
scripts/memcheck.sh images esp32c3 out && scripts/memcheck.sh analyze esp32c3 out   # task pools, stack, frames
```

The minimum free wireless heap comes from `rohi_net_esp::wireless_heap()` (the firmware logs it with its snapshot; the
`heap-stats` feature adds the minimum).

## Persistent logging (`rohi-logging`)

Release build of `altruist-urban`, measured with `nm -S` on the ELF (`cargo build-esp32c3` / `cargo build-esp32c6`):

| Item | ESP32-C3 | ESP32-C6 |
|------|---------:|---------:|
| `altruist_urban::esp::logging::LOGGING` (queue, batch, streams, status; initialized data in RAM) | 12 512 B | 12 512 B |
| `altruist_urban::esp::logging::logging_task` future (`.bss`) | 2 304 B | 2 304 B |

The limits are in `firmwares/altruist-urban/src/logging.rs` (`QUEUE_DEPTH` 24 records, `READ_BYTES` 2 048 B per
open `read` stream, at most two streams) and in `rohi_logging` (batch of 2 KiB of text, one compressed block of at most 2 056 B). Flash:
the `rohi-log` partition is 256 KiB of the 4 MB / 8 MB flash (the `factory` partition shrank by that amount).

**Not measured:** the share of `.text` the logging code adds, stack depth of the logging task while it compresses (the
LZ4 compressor keeps a 2 KiB table and the writer a 2 KiB chunk on the stack), the CPU time of a flush, and how long a
flash sector erase stalls Wi-Fi, BLE and the microphone DMA ring. These need a device.
