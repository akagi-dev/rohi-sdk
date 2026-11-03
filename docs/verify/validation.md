# Validation

Validation is how a claim gets evidence. ROHI distinguishes the levels and never lets one stand in for another:
compilation is not hardware validation.

| Level | How | Where |
|-------|-----|-------|
| Host tests | `cargo test`, in-memory devices and mocked buses, no hardware | [Host testing](../validation/host-testing.md) |
| Target builds | `cargo check-esp32c3`, `cargo check-esp32c6` and the firmware builds in CI | [Targets and features](https://docs.rs/rohi-sdk/latest/rohi_sdk/portability/index.html#targets-and-features) |
| Resource budget | measured flash, RAM and stack | [Resource budget](../validation/resource-budget.md) |
| Hardware | a recorded run on a physical board | [Hardware testing](../validation/hardware-testing.md) |

## Validating on hardware

1. Build and flash the firmware or an example for your board ([Quick start](../build/quick-start.md)).
2. Follow the procedure and checklist of [Hardware testing](../validation/hardware-testing.md) for the capability you are checking.
3. Record the result: device and revision, firmware revision, procedure, outcome and logs, in a report you can link.
4. Add the result to the [support matrix](../support/overview.md#maintenance-policy): change
   `hardware_verification` and attach the report as `hw-report` evidence. The validator rejects `verified` or `failed`
   without it.

## Sharing results

A hardware report can be a pull request that adds the report and updates the matrix, or an issue with the same content.
A failure is as valuable as a success and is recorded as `failed`.
