# Add a control operation or an action

The control plane is generic: **you almost never touch the protocol**. Decide what you are adding, then use the matching
recipe. The wire contract is in [Control plane](../sdk/control.md), the model in
[Service pipeline](../sdk/service.md).

| You want a client to... | It is a... | Recipe |
|-------------------------|-----------|--------|
| set how a module behaves, persistently | **config section** | [Add a ConfigSection](add-config-section.md), step 6 |
| see what a module is doing now | **service status** | [Add a service](add-service.md) |
| make a module do something (scan, reconnect, provision) | **action** | below, and [Add a service](add-service.md#3-actions-optional) |
| something that fits none of these | a new protocol operation | [A new request](#a-new-request) |

## An action

Actions are declared by a service, so a new operation is one entry in its `ACTIONS` and one arm in `invoke`:

1. **Describe it** in `ManagedService::ACTIONS` with `ActionDescriptor::new(id, "name")` (a stable numeric id, never reused), typed `args`
   (all required, at most 3) and `results` (the fields of each returned record, at most 4 fields, 6 records).
2. **Privilege**: `.privileged()` if it changes anything (the link policy must grant write access), `.secret()` if an
   argument is a secret (implies privileged, requires the strongest privilege, and the argument is overwritten after the
   call). A read-only action such as a scan needs neither.
3. **Implement it** in `invoke(action, args, out)`, where `action` is the numeric id. The registry has already checked that the arguments exist and match
   the declared kinds. Write each result record with `out.next_record()` and the typed writers (`out.text(id, ..)`,
   `out.i64(id, ..)`); a field that does not match its descriptor is refused. Return `ActionError::Rejected("code")` for a
   refusal a client may react to; the code is stable and machine readable (`already_provisioned`).
4. **Mutation rule**: if the action commits durable state that a runtime must react to, the commit **and** the
   publication happen in the owner task ([Durable mutation](../concepts/async-and-cancellation.md#durable-mutation)).
   Long-running actions bound their own wait (see the Wi-Fi scan timeout) and are cancellation safe.
5. **Tests**: the action against the registry (`ServiceRegistry::invoke`: arguments checked, records, refusals), and
   through the dispatcher if the privilege matters (`rohi-control` tests show the policy gate).
6. **Docs**: nothing in the protocol; add the action to the service's page. `rohi action <service>` lists it.

No protocol, dispatcher, client or CLI change is needed.

## A new request

Only when the operation fits none of the planes (config, status, actions, info):

1. **Schema** (`crates/rohi-control-proto/proto/rohi/control/v1/control.proto`): add the request and response messages
   and a `oneof` entry with a **new field number**. Add bounds for every repeated, string and bytes field in `build.rs`
   so the generated types stay heapless, and keep the memory budget test green (`messages_stay_small_in_memory`): a
   response is built on the stack of a microcontroller, so prefer **paging** to a larger response. Do not bump
   `PROTOCOL_VERSION`; the protocol is under development.
2. **Dispatch** (`rohi-control/src/controller.rs`): handle the call in `execute`, decide its `Privilege` in `dispatch`,
   and map types in `mapping.rs`; protobuf types do not leak into other crates. Failures map to the typed errors
   (`INVALID_ARGUMENT`, `NOT_FOUND`, `BUSY`, `UNAVAILABLE`, `PERMISSION_DENIED`, `CONFLICT`, `REJECTED`, `INTERNAL`);
   secrets are never returned or logged.
3. **Backend**: if the dispatcher needs something from the firmware, add it to `ControlBackend` (the dispatcher never
   sees stores, flash or radio drivers) and update the implementations: `Device`, the fakes in `rohi-control-ble` and
   `tools/rohi` tests.
4. **Client**: add the generic method to `rohi::Device` and, if useful, a CLI subcommand.
5. **Tests**: protocol golden bytes and bounds, dispatcher (authorization, validation, "nothing stored on invalid
   input"), loopback client end to end, and a cancellation test if the operation is long-running.
6. **Docs**: the request table and limits in [Control plane](../sdk/control.md), resource limits if any, and the
   [glossary](../reference/glossary.md) for new terms.
