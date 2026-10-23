# Add a control operation

The control plane is one protobuf request and one response per operation, dispatched by `rohi-control` and served over
BLE. The wire contract is documented in [Control plane](../sdk/control.md); this is the recipe for extending it.

1. **Schema** (`crates/rohi-control-proto/proto/rohi/control/v1/control.proto`): add the request/response messages and a
   `oneof` entry with a **new field number**. Field numbers and enum values are stable and only ever added. Add bounds
   for every repeated/bytes field in `build.rs` so the generated types stay heapless, and update the worst-case size
   tests if the message can grow beyond `MAX_REQUEST_LEN` / `MAX_RESPONSE_LEN`. Do not change `PROTOCOL_VERSION`
   for additive changes.
2. **Capability and privilege** (`rohi-control`): add a `Capabilities` flag if clients must be able to discover the
   feature, extend `requirements()` in `controller.rs` with `(capability, Privilege::Read | Mutate)` and handle the call
   in `execute`. Mutating operations need `Privilege::Mutate` (checked by the `AuthorizationPolicy`).
3. **Service seam**: add a method to `ControlServices` for what the firmware must provide (`async` or sync), with
   documented cancellation semantics. The dispatcher never sees stores, flash or radio drivers. Update every
   implementation: the firmware adapter (`UrbanControlServices`), the fakes in `rohi-control`, `rohi-control-ble` and
   `tools/rohi` tests.
4. **Mutation rule**: if the operation commits durable state that a runtime must react to, the commit **and** the
   publication happen in the owner task ([Durable mutation](../concepts/async-and-cancellation.md#durable-mutation)),
   exposed as one service call (see `set_wifi_config`).
5. **Errors**: map service failures to the typed protocol errors (`INVALID_ARGUMENT`, `NOT_FOUND`, `NOT_SUPPORTED`,
   `BUSY`, `UNAVAILABLE`, `PERMISSION_DENIED`, `INTERNAL`); secrets are never returned or logged.
6. **Mapping**: convert between `rohi-*` types and protobuf in `rohi-control/src/mapping.rs`; the protobuf types do not
   leak into other crates.
7. **Client**: add the typed method to `rohi::Device` (it checks capabilities first) and a CLI subcommand if useful.
8. **Tests**: protocol golden bytes/bounds, dispatcher (capability, authorization, validation, "nothing stored on invalid
   input"), loopback client end to end, and a cancellation test if the operation is long-running.
9. **Docs**: the request table and limits in [Control plane](../sdk/control.md), resource limits if any, and the
   [glossary](../reference/glossary.md) for new terms.
