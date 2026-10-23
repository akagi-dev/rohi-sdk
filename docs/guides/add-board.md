# Add or support a board

A board is a place where pins and buses are named. `rohi-hal-esp` is the single place for that; drivers, firmware tasks
and the portable crates never contain pin numbers. Existing example: `rohi_hal_esp::board::altruist_urban`
([Boards](../platform/boards.md)).

**A new chip variant of an existing board** (for example another ESP32 for the Altruist Urban):

1. Add `esp32xx.rs` next to `esp32c3.rs` / `esp32c6.rs` in `board/altruist_urban/` with the pin map and the peripheral
   types (`Hardware`), and select it with a Cargo feature forwarded by the firmware and by `rohi-hal-esp`,
   `rohi-config-esp`, `rohi-net-esp` and the other platform crates.
2. Add the matching `rohi-net-esp` / `esp-radio` feature, a partition table
   (`firmwares/altruist-urban/partitions-<chip>.csv`) mirrored by `rohi_config_esp::layout` (a host test keeps them in
   sync), and the Cargo alias plus runner entry in `.cargo/config.toml` (`check-*`, `build-*`).
3. Add the chip to the CI matrix, and to [Targets and features](../reference/targets-and-features.md). Chips are never
   validated together with `--all-features`.

**A new board revision** with a different pin map gets its own module instead of an edit of the existing tables.

**A new board** (new product):

1. Create `board/<name>/` in `rohi-hal-esp` exposing a chip independent API: `Hardware::split(peripherals)` and
   `<Board>::new(hardware)` returning configured buses (I²C, UART, I²S), ready for the sensor drivers.
2. Create `firmwares/<name>/` composing the portable crates. Put its static resource sizes in its own
   `src/resources.rs` with the documentation of [Memory model](../concepts/memory-model.md); the product owns its budget.
3. Document the pin map and its verification status in [Boards](../platform/boards.md), marking anything that has not
   been verified on hardware.

Keep vendor types inside the platform crate and the portable APIs unchanged; if a portable crate needs to change for the
new board, the seam was leaking ([Portability](../concepts/portability.md)).
