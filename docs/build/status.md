# Status

**Status** is what the device is doing now: the lifecycle **state**, the **health**, whether it is **ready**, a few
metrics and the **active issues**. It is read-only and describes live state, not stored data.

## A small example

A service implements `ManagedService` over a handle to its runtime state:

```rust,ignore
impl<M: RawMutex> ManagedService for LocationService<'_, M> {
    const ID: u32 = 7;                              // stable numeric id
    const NAME: &'static str = "location";          // stable public name
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(1, "latitude", ValueKind::F32).unit(Unit::Degrees),
    ];

    async fn summary(&self) -> ServiceSummary { /* state, health, ready */ }
    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) { /* what is wrong now */ }
    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> { /* values */ }
}
```

- **State, health and readiness are three values**, not one enum: a connecting Wi-Fi is `starting / unknown / not ready`,
  a telemetry service that cannot reach its backend is `running / degraded / not ready`, and a disabled optional service
  is `disabled`, which is not an error.
- **Issues** are problems that exist now, with a stable numeric code and a severity. There is no message text and no
  history.
- The SDK adds a **status revision** and the time in the current state; the **device health** is decided by the firmware.

Once the service is in the firmware's `Services` tuple, `rohi status` lists it and `rohi status location` shows its
values and issues. How users read this is [Status & diagnostics](../use/desktop/status.md).

## Go deeper

| Topic | Page |
|-------|------|
| Step by step | [Add a service](../guides/add-service.md) |
| Traits, limits, device health rules | [Service pipeline](../sdk/service.md) |
| A complete small example | [Location](../sdk/location.md) |
