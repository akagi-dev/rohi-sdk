# Status

**Status** is what the device is doing now: the **health**, a few metrics and the
**active issues**. It is read-only and describes live state, not stored data.

- **Health is the one common indicator**: `unknown`, `ok`, `degraded` or `error`. A connecting Wi-Fi is `unknown`,
  a telemetry service that cannot reach its backend is `degraded`, and a disabled optional service is `unknown`, which
  is not an error. What more a service has to tell (a link state, a sensor state) is a field of its own.
- **Issues** are problems that exist now, with a stable numeric code and a severity. There is no message text and no
  history.
- Values are typed fields with stable ids and units; a value that does not exist now is simply absent.
- The SDK adds a **status revision**, which changes with the health or the active issues; the **device health** is decided by the firmware.

Once a service is registered in the firmware, `rohi status` lists it and `rohi status location` shows its values and
issues. How users read this is [Status & diagnostics](../use/desktop/status.md).

## Go deeper

| Topic | Page |
|-------|------|
| Step by step | [Add a service](../guides/add-service.md) |
| Contracts, limits, device health rules | [`rohi-service` rustdoc](https://docs.rs/rohi-service) |
| A complete small example | [`rohi-location` rustdoc](https://docs.rs/rohi-location) |
