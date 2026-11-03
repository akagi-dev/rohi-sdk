# Add a service

A service makes a runtime part of the device visible and controllable: its **state** (`rohi status <name>`), optionally
its **configuration** (`rohi config <name> ...`) and its **actions** (`rohi action <name> ...`). It needs **no change**
to the protocol, the dispatcher, the client or the CLI: you implement the `rohi-service` traits in the crate that owns
the runtime and add the result to the firmware. The contracts are in the
[`rohi-service` rustdoc](https://docs.rs/rohi-service); the Location service
([`rohi-location`](https://docs.rs/rohi-location)) is a small complete example.

## 1. Status

Give the service a name with `#[derive(Service)]` (its numeric id comes from the name) and implement `Status` over a
**handle** to the live runtime state, without a duplicated status struct. A good status:

- keeps **state, health and readiness separate**: a disabled service is not an error, a starting one is not ready yet,
  a working one with reduced capability is degraded;
- reports **issues** as currently active problems with a stable numeric code and a severity, never message text or
  history;
- gives every field a **stable id and unit**; a value that does not exist now is simply not written;
- is a cheap, non-blocking snapshot: do not wait on slow hardware inside it.

The SDK adds the status revision and the time in the current state; the firmware decides the device health.

## 2. Config (optional)

If users configure it, add a section as in [Add a ConfigSection](add-config-section.md). Name the section like the
service when they belong together (`location`, `connectivity`, `wifi`).

## 3. Actions (optional)

Operations the service offers implement `Action`, see [Add a control operation or an action](add-control-operation.md).

## 4. Register in the firmware

In `firmwares/altruist-urban/src/control.rs` add the service instance to the firmware's services, construct it in
`device()` and add a reference to the status tuple if it reports status and to the action tuple if it offers actions
(the Embassy control task needs the concrete types). A duplicate id is a build error. A config section is not added
there but in `firmware_configs!` (see [Add a ConfigSection](add-config-section.md)). If the runtime must react to a
stored section, the configuration owner publishes it in the commit step through the section's binding.

## 5. Tests

- The feature crate: the status in every state (summary, issues, values) and the schema, and for a section the
  validation and a golden test of the stored bytes.
- End to end: `tools/rohi/tests/common/mod.rs` builds the fake device from the real sections and services; add yours
  there and test `rohi config <name> FIELD=VALUE` and `rohi status <name>` against it.

## 6. Docs

Document the service in the rustdoc of its crate (what it reports, issue codes, fields), list it in
[Status & diagnostics](../use/desktop/status.md), and update the [SDK crates](../build/sdk-crates.md) page if it is a
new crate.
