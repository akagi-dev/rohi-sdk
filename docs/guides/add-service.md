# Add a service

A service makes a runtime part of the device visible and controllable: its **state** (`rohi status <name>`), optionally
its **configuration** (`rohi config ... <name>`) and its **actions** (`rohi action run <name> ...`). It needs
**no change** to the protocol, the dispatcher, the client or the CLI: you implement `rohi-service` traits in the crate
that owns the runtime and add the result to the firmware's tuples. [Location](../sdk/location.md) is a small complete
example (config section, service, shared state, telemetry); [Service pipeline](../sdk/service.md) explains the model.

## 1. Status

Implement `ManagedService` in the feature crate, over a **handle** to the live runtime state (no duplicated status
struct):

```rust,ignore
pub struct LocationService<'a, M: RawMutex>(pub &'a LocationShared<M>);

impl<M: RawMutex> ManagedService for LocationService<'_, M> {
    const ID: u32 = 7;                              // stable numeric id, the address of requests
    const NAME: &'static str = "location";          // stable public name
    const VERSION: u16 = 1;                         // version of the status schema
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(1, "latitude", ValueKind::F32).unit(Unit::Degrees),
        // ...ids are stable: only ever add, never reuse or renumber
    ];
    const ESSENTIAL: bool = false;                  // does the device depend on it?

    async fn summary(&self) -> ServiceSummary { /* state, health, ready */ }
    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) { /* active problems */ }
    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> { /* values that exist now */ }
}
```

Rules that make the status useful:

- **State, health and ready are separate.** `disabled` is not an error (turned off, or never set); `starting` is
  `unknown`/not ready; `running` + `degraded` means it works with reduced capability. Do not invent one status enum.
- **Issues are active problems with a stable numeric `code`** (named once in `ISSUES`, `snake_case`, never renumbered or
  reworded) and a severity; there is no message text. Only what is wrong *now*; history is diagnostics. At most 4 active
  and 12 described.
- **Fields have stable ids and units.** A value that does not exist now is simply not written. At most 20 fields per
  service; names are dotted paths for nesting (`bmx280.temperature`); strings and bytes are cut at 32 bytes; floats are `f32`.
- `ESSENTIAL = false` limits how much a failing service hurts the **device health**, which the firmware decides (the
  default policy: `rohi-service::default_device_health`).
- Status is a snapshot of live state, cheap and non-blocking; do not wait on slow hardware inside it.

The registry wraps the service in `Tracked`, which adds the **status revision** (it changes when state, health, readiness
or issues change) and the time in the current state. You do nothing for that.

## 2. Config (optional)

If users configure it, add a `ConfigSection` and implement `ManagedConfig` as in
[Add a ConfigSection](add-config-section.md): the schema, typed conversion, whole-section validation.
Name the section like the service when they belong together (`location`, `connectivity`, `wifi`).

## 3. Actions (optional)

Operations the service offers go into `ACTIONS` and `invoke`, see [Add a control operation or an action](add-control-operation.md).

## 4. Register in the firmware

In `firmwares/altruist-urban/src/control.rs` add the service to the `Services` tuple and its section to `ConfigSections`
(each takes up to 8 entries), and construct them in `device()`:

```rust,ignore
pub type Services = (
    Tracked<NetworkService<'static, RawMutex, net::Probe>, RawMutex>,
    // ...
    Tracked<LocationService<'static, RawMutex>, RawMutex>,
);
// device(): (..., Tracked::new(LocationService(location::shared())), ...)
```

If the runtime must react to a stored section, the configuration owner publishes it in the commit step
(`config.rs`, `publish_committed`, and once at boot).

## 5. Tests

- The feature crate: the status of every state (summary, issues, values), the schema (ids unique, names fit 24 bytes,
  at most 20 fields), and for a section the typed validation and a golden test of the stored bytes.
- End to end: `tools/rohi/tests/common/mod.rs` builds the fake device from the **real** sections and services; add yours
  there and test `rohi config set`, `rohi status <name>` against it.

## 6. Docs

A page for the service in `docs/sdk/` (what it reports, issue codes, fields), the table in
[Service pipeline](../sdk/service.md#what-lives-where), the services list in
[Services and status](../getting-started/services.md), and the [crate map](../concepts/crate-map.md).
