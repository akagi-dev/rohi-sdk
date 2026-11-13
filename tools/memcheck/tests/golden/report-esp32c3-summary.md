# Task Memory & Stack Budget

| chip | outcome | `.bss` | `.data` | task pools | main stack | largest frame |
|---|---|---:|---:|---:|---:|---:|
| ESP32-C3 | FAIL | 40600 B (+600 B) | 1000 B | 27480 B (+600 B) | 22024 B | 2400 B |

### ESP32-C3

- budget `max_task_ram`: close to the limit (limit 30000 B, actual 27480 B)
- budget `growth:task:app::control::control_task::POOL`: **FAIL** (limit 500 B, actual 600 B)
- budget `growth:frame:rohi_logging::service::collect`: **FAIL** (limit 200 B, actual 300 B)
- task `app::control::control_task::POOL`: 17440 B → 18040 B (+600 B)
- section `.bss`: +600 B
- frame growth `rohi_logging::service::collect`: 2100 B → 2400 B (+300 B)

A frame is the stack of one function, not the stack depth of a task. `NEEDS HARDWARE DATA` means the static evidence is within the budgets and no stack peak is measured.
