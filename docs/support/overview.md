# Reading the support matrix

The support matrix is the authoritative statement of **what ROHI supports, on which board, with which firmware, and what
has been verified**. It is published in two forms generated from one source:

| Form | Location | For |
|------|----------|-----|
| Machine-readable | [`device-matrix.toml`](https://github.com/akagi-dev/rohi-sdk/blob/master/device-matrix.toml) | tools and AI assistants |
| Human-readable | [Support matrix](matrix.md) | users and developers |

> Hardware that exists is not the same as hardware ROHI supports, and compilation is not the same as hardware
> validation. The matrix never merges the layers below into one "supported" flag.

## The layers

1. **Upstream hardware.** Designs, schematics and datasheets live in [airalab/hardware](https://github.com/airalab/hardware).
   The matrix links to them and never copies them.
2. **Other firmware.** [airalab/altruist-firmware](https://github.com/airalab/altruist-firmware) is an independent
   firmware with its own devices and features. Its claims are listed as `external` and are not ROHI support.
3. **ROHI SDK.** Drivers and services in `crates/` (*implementation*).
4. **ROHI firmware.** What a firmware in `firmwares/` actually starts (*integration*).
5. **Software validation.** Host tests, simulation and CI target builds.
6. **Hardware validation.** A recorded run on a physical board.

## Status definitions

Every entry describes one capability on one board with one firmware.

| Field | Values | Meaning |
|-------|--------|---------|
| `board_part` | `documented`, `undocumented`, `not-on-board`, `not-applicable` | Whether the board carries the hardware the capability needs, according to upstream documentation. `undocumented` means upstream does not say; `not-applicable` is for services without a dedicated part. |
| `implementation` | `implemented`, `partial`, `not-implemented`, `external` | Whether ROHI implements the capability. `external` means only another firmware project does. |
| `integration` | `integrated`, `not-integrated`, `not-applicable` | Whether this firmware starts it. |
| `host_tests` | `passing`, `none` | Tests that run in CI without hardware (`cargo test`). |
| `simulation` | `yes`, `none` | Peripheral-level emulation of the device. Host tests with mocked buses are **not** simulation. |
| `target_build` | `ci-built`, `not-built`, `not-applicable` | The firmware builds for the board's chip in CI. |
| `hardware_verification` | `verified`, `failed`, `not-verified`, `not-applicable` | A recorded run on a physical board. |

`not-verified` means **no evidence either way**: it is neither success nor failure. `verified` and `failed` require
`hw-report` evidence: what device and revision, which firmware revision, the procedure and the result.

Each entry also carries `limitations` (calibration, units, accuracy, incomplete behaviour) and `evidence`, links to source
code, tests, CI, hardware reports or upstream documentation. The kinds of evidence are `source`, `test`, `ci`,
`hw-report` and `upstream`.

A board has one of three `rohi_support` values: `supported` (a ROHI firmware targets it), `other-firmware-only` (only
another project's firmware targets it) and `not-integrated` (SDK pieces may exist, nothing targets it).

## Common questions

These answers come from the matrix at the time of writing; the [matrix](matrix.md) is authoritative.

**Does ROHI support Altruist Insight?** No. The Insight is documented upstream and the independent altruist-firmware
targets it, but ROHI has no board module, firmware or display driver for it. Only the SCD4x driver exists in the SDK.

**Can I measure CO₂ with Altruist Urban?** Not on the stock board: the Urban has no SCD4x. The ROHI Urban firmware
starts the SCD4x driver, so a sensor on the I²C bus might work, but upstream does not document how to attach one and it is
not verified.

**Which Altruist Urban capabilities have been tested on ESP32-C6 hardware?** None are recorded as verified. Every
capability has host tests and a CI build for the C6, and the C6 pin map comes from the upstream schematic and is not
verified on hardware.

**Can I measure environmental noise, and how is it calibrated?** Partly. The level is unweighted dB SPL (not dBA) from
the nominal ICS-43434 sensitivity, with no per-device calibration, so it is not comparable to a calibrated sound level
meter. See [Sensors](https://docs.rs/rohi-hal/latest/rohi_hal/sensors/index.html).

**Can I build a water-quality monitor with the existing firmware?** No. There are no water-quality sensor drivers, board
or firmware. The SDK services (configuration, Wi-Fi, telemetry, BLE control) can be reused by a new firmware.

## Maintenance policy

- A pull request that changes a board module, a sensor driver, a firmware's wiring or a hardware result **updates the
  matrix in the same pull request**.
- A hardware result is added only together with a hardware report: add an `evidence` item of kind `hw-report` with
  the board revision, the firmware revision, the procedure and the result.
- After editing `device-matrix.toml`, regenerate the page with `cargo run -p rohi-devices -- render --write`.
- Set `reviewed` whenever the data is reviewed against upstream.
- `cargo test -p rohi-devices` (part of `cargo test` in CI) checks the schema, that every referenced board,
  firmware, capability and evidence exists, that repository paths exist, that every sensor driver in
  `crates/hal/src/sensors` has a capability, that the claims of an entry do not contradict each other and that the
  rendered page is current.
- Upstream links point to a fixed commit of airalab/hardware; move them forward when re-reviewing.
