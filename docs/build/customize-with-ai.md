# Customize firmware with AI

An AI coding assistant is a good fit for ROHI firmware: the SDK is built from small, typed components with written
contracts, a support matrix says what is verified, and a handful of commands tell the assistant (and you) immediately
whether a change holds together. This page describes a workflow that keeps the assistant useful and honest, with prompts
you can copy.

> The assistant writes code; **you** verify it on hardware. Compiling, passing host tests and a CI build are not
> hardware validation, and an assistant must not claim otherwise. See [Validation](../verify/validation.md).

## What to give the assistant

Open the `rohi-sdk` repository (or a repository that depends on it) in the assistant and tell it where the facts are:

| Source | What it answers |
|--------|-----------------|
| [Support matrix](../support/overview.md) (`support-matrix/matrix.toml`) | what ROHI supports per board, firmware and capability, and what is verified |
| [Create & customize](create-and-customize.md) guides (`docs/guides/`) | the step-by-step procedure for each kind of change |
| [Core concepts](core-concepts.md) | what Config, Status and Action are |
| [`firmwares/altruist-urban`](https://github.com/akagi-dev/rohi-sdk/tree/master/firmwares/altruist-urban) | the reference composition to copy from |
| [`examples/`](https://github.com/akagi-dev/rohi-sdk/tree/master/examples/src/bin) | one small program per sensor |
| the crate rustdoc (`cargo doc --no-deps`) | APIs and contracts |
| [Targets and features](../reference/targets-and-features.md) | the exact build commands per chip |

A short standing instruction in your assistant's project instructions file helps. This one is a starting point:

```text
This is a Rust no_std firmware workspace for ESP32-C3 and ESP32-C6 built on the ROHI SDK.
- Before changing anything read the matching guide in docs/guides/ and copy the structure of
  firmwares/altruist-urban; do not invent APIs, read the crate source.
- Hardware facts (boards, pins, sensors) come only from support-matrix/matrix.toml and
  docs/platform/boards.md. If a fact is missing, say so; never guess pin numbers.
- Never claim that something works on hardware. Report what you ran: cargo test,
  cargo check-esp32c3, cargo check-esp32c6.
- Drivers contain no pin numbers. Application code does not allocate. Config field ids are
  only ever appended, never reused or renumbered.
- Keep changes small and update the support matrix and the docs in the same change.
```

## The loop

1. **Ask for a plan first.** Have the assistant name the files it will touch and the guide it follows. Reject plans that
   edit the SDK crates when a firmware change is enough.
2. **Make one small change.** One sensor, one setting, one action.
3. **Let it run the checks** and read the result yourself:

   ```bash
   cargo test                                   # host tests, no hardware
   cargo check-esp32c3 && cargo check-esp32c6   # both chips type-check
   cargo clippy --all-targets -- -D warnings
   ```

4. **Flash and look.** Run it on the board, then `rohi status`, `rohi config`, `rohi action`: a correct change shows up
   in the generic client without any client change ([Desktop application](../use/desktop/index.md)).
5. **Record the result**, success or failure, in the [support matrix](../support/overview.md#maintenance-policy).

## Prompts

Replace the names in brackets. Each prompt follows the guide linked next to it.

### Add a sensor

[Add a sensor](../guides/add-sensor.md)

```text
Add support for the [SCD41] sensor to the altruist-urban firmware.
Follow docs/guides/add-sensor.md. First check support-matrix/matrix.toml and tell me
whether the Altruist Urban has this part and what is already implemented. Reuse the
existing driver in crates/hal if there is one. Show me the plan and the files you will
change before editing. Afterwards run cargo test and cargo check-esp32c3 and
cargo check-esp32c6, and update the support matrix without marking anything verified.
```

### Add a setting

[Add a ConfigSection](../guides/add-config-section.md)

```text
Add a configuration section `sampling` with a field `interval_s` (u32, default 60,
allowed 10..=3600) and use it as the poll interval of the [SDS011] sensor task.
Follow docs/guides/add-config-section.md: typed section, ConfigOverrides, stable field
ids, whole-section validation, golden test of the stored bytes, ManagedConfig so that
`rohi config sampling` works. Do not change the control protocol or the rohi tool.
```

### Add a status service

[Add a service](../guides/add-service.md)

```text
Add a service `[battery]` that reports state, health, readiness and the field
`voltage` in volts, with an issue `low_voltage` below [3.3] V. Follow
docs/guides/add-service.md and copy the structure of the location service. Register it
in the Services tuple in firmwares/altruist-urban/src/control.rs. Add host tests for
every state and show me the expected `rohi status [battery]` output.
```

### Add an action

[Add a control operation or an action](../guides/add-control-operation.md)

```text
Add an action `calibrate` to the `[sensors]` service that takes the argument
`reference_ppm` (u64) and is privileged. Follow docs/guides/add-control-operation.md,
check cancellation safety, and add a test through ServiceRegistry::invoke including the
refusal case with a stable reason code.
```

### Remove or trim a firmware

```text
I only need the sensors and BLE control, no Wi-Fi, time or telemetry. List what
firmwares/altruist-urban starts in main.rs and what depends on what, propose the
smallest set of removals, and check that cargo check-esp32c3 still passes. Do not
change anything in crates/.
```

### Support a new board

[Add or support a board](../guides/add-board.md)

```text
I have a board with [ESP32-C6, BME280 on I2C GPIO6/GPIO7, no other sensors].
Follow docs/guides/add-board.md: a new board module in crates/hal-esp and a new
firmware crate, with its own resource budget. Take the pins only from what I give you
here. Add the board to the support matrix with every hardware verification set to
not-verified and list what I have to test on the device.
```

### Review a change

```text
Review the diff against docs/guides/add-config-section.md and
the SDK rules: no allocation in application code, no pin numbers in
drivers, field ids only appended, cancellation-safe async, no secrets logged. List
violations only, with file and line.
```

### Explain a failure

```text
`cargo check-esp32c6` fails with the following error: [paste]. Explain the cause in
terms of the ROHI crates involved and propose the smallest fix. Do not disable a check
or add an allow attribute.
```

## Pitfalls

- **Hardware facts.** Assistants invent pin numbers and part numbers. Pin maps live in
  [Boards](../platform/boards.md); verified status lives in the [support matrix](../support/matrix.md).
- **Compiles is not works.** A passing build says nothing about wiring, timing or calibration.
- **Wrong layer.** A new feature usually belongs in a feature crate plus a firmware registration, not in the control
  protocol or the desktop tool; those need no change ([Create & customize](create-and-customize.md)).
- **Stable ids.** Field ids, action ids, issue codes and section ids are part of the stored and wire contract. Do not
  let an assistant renumber or rename them.
- **Secrets.** Never paste Wi-Fi passwords or device keys into a prompt.
