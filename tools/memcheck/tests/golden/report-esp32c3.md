# Task Memory & Stack Budget

## ESP32-C3 — Task Memory & Stack Budget

Build: toolchain `rustc 1.99.1`, commit `abc123`, target `riscv32-esp32c3`, profile `firmware`, features `esp32c3`.

### RTOS threads / physical stacks

| name | allocated | peak used | headroom | source |
|---|---:|---|---|---|
| rtos/main | 22024 B | not measured (hardware only) | not measured | linker symbols `_stack_start` / `_stack_end` |
| native RTOS threads (radio, network, application) | not_detectable_statically | not measured | not measured | Threads are created at run time with stacks from the heap; the ELF symbols are not a complete list. Use a run-time inventory from hardware. |
| interrupt and executor contexts | runtime_only | not measured | not measured | Not distinguishable from the image; the Embassy tasks share the main stack. |

`rtos/main` grows down from `0x3fcce400` to `0x3fcc8df8`; below it: `.bss` (an overflow corrupts it first). Overflow guard evidence: none found.

### Embassy tasks / future pools

| name | reserved RAM | pool slots | stack owner | confidence | Δ base |
|---|---:|---|---|---|---:|
| ble_task | 7064 B | unknown | rtos/main | exact | n/a |
| control_task | 17440 B | unknown | rtos/main | exact | n/a |
| logging_task | 2304 B | unknown | rtos/main | exact | n/a |
| reboot_task | 72 B | unknown | rtos/main | exact | n/a |

Total: 26880 B in 4 tasks (0 unknown). In `.bss`: 26880 B of 40000 B (other 13120 B); in `.data`: 0 B of 1000 B (other 1000 B).

### RAM sections

| section | size | Δ base |
|---|---:|---:|
| .bss | 40000 B | n/a |
| .data | 1000 B | n/a |
| .stack | 22024 B | n/a |
| static total (data + bss + noinit) | 41000 B | |

Largest statics:

| symbol | section | size |
|---|---|---:|
| `app::control::control_task::POOL` | .bss | 17440 B |
| `app::control::ble_task::POOL` | .bss | 7064 B |
| `app::logging::logging_task::POOL` | .bss | 2304 B |
| `app::reboot::reboot_task::POOL` | .bss | 72 B |

### Function stack frames

A frame is the stack of one function. It is not the stack depth of a task: call chains, inlining, dynamic dispatch, async suspension and interrupts are not analyzed.

Coverage: 3 of 4 functions have frame metadata (75.0 %). Functions without it (assembly, ROM and vendor blobs) have an unknown frame.

| frame | function |
|---:|---|
| 2100 B | `rohi_logging::service::collect` |
| 1400 B | `rohi_control::controller::handle` |
| 48 B | `app::main` |

Focus `control_dispatch` (1 functions with frame data):

| frame | function |
|---:|---|
| 1400 B | `rohi_control::controller::handle` |

Focus `logging` (1 functions with frame data):

| frame | function |
|---:|---|
| 2100 B | `rohi_logging::service::collect` |

### Budgets

| budget | limit | actual | result |
|---|---:|---:|---|
| max_data | 2000 B | 1000 B | pass |
| max_bss | 50000 B | 40000 B | pass |
| max_task_ram | 30000 B | 26880 B | pass |
| max_frame | 4000 B | 2100 B | pass |
| min_main_stack | 20000 B | 22024 B | pass |

**Outcome: NEEDS HARDWARE DATA**

The static evidence is within the budgets. The stack peak is not measured: attach hardware observations (see `docs/validation/resource-budget.md`).
