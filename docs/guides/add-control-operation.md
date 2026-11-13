# Add a control operation or an action

The control plane is generic: **you almost never touch the protocol**. Decide what you are adding, then use the matching
recipe. The wire contract is in the [`rohi-control` rustdoc](https://docs.rs/rohi-control) and
[`rohi-control-proto`](https://docs.rs/rohi-control-proto).

| You want a client to... | It is a... | Recipe |
|-------------------------|-----------|--------|
| set how a module behaves, persistently | **config section** | [Add a ConfigSection](add-config-section.md) |
| see what a module is doing now | **service status** | [Add a service](add-service.md) |
| make a module do something (scan, reconnect, provision) | **action** | below |
| something that fits none of these | a new protocol operation | [A new request](#a-new-request) |

## An action

An action is a capability of a service, so adding one is a change inside the service's crate; see the
[`rohi-service` rustdoc](https://docs.rs/rohi-service) for the contract.

1. **Describe it** in the service's action table: a stable numeric id that is never reused, a name, typed arguments (all
   required) and the fields of the records it returns.
2. **Declare the access.** Every action states what the link must be allowed to do: `ActionAccess::Read` for a scan,
   `Mutate` for an action that changes the runtime or the configuration, `Provision` for one that changes what
   identifies the device (the identity, the whole configuration storage). A secret argument is a separate property
   (`.secret()`): it only makes the argument wiped after the call and never logged, and it does not change the access.
3. **Implement it** in the service's `invoke`. The arguments are already checked against the description. Refuse with a
   stable, machine readable reason that a client may react to.
4. **Mutation rule.** If the action commits durable state that a runtime must react to, the commit and the publication
   happen in the owner task. Long-running actions bound their own wait and must be cancellation safe, see the
   [cancellation rules](https://docs.rs/rohi-sdk/latest/rohi_sdk/cancellation/index.html).
5. **Tests**: the action against the registry (arguments checked, records, refusals) and, if the privilege matters,
   through the dispatcher.
6. **Docs**: the rustdoc of the service. `rohi action <service>` lists the action and generates its help from the
   device; no protocol, dispatcher, client or CLI change is needed.

## A new request

Only when the operation fits none of the planes (config, status, actions, info):

1. **Schema** in `crates/control-proto/proto/rohi/control/v1/control.proto`: add the messages and a `oneof` entry with a
   **new field number**; bound every repeated, string and bytes field in `build.rs` and keep the memory budget test
   green. Prefer paging to a larger response. Do not bump the protocol version; it is under development.
2. **Dispatch** in `rohi-control`: handle the call, decide its privilege and map the types; protobuf types do not leak
   into other crates, and secrets are never returned or logged.
3. **Backend**: if the dispatcher needs something from the firmware, add it to the control backend and update its
   implementations (the device, the fakes in `rohi-control-ble` and the `tools/rohi` tests).
4. **Client**: add the generic method to the `rohi` library and, if useful, a CLI subcommand.
5. **Tests**: protocol golden bytes and bounds, dispatcher authorization and validation, an end-to-end loopback, and a
   cancellation test if the operation is long-running.
6. **Docs**: the rustdoc of `rohi-control` and `rohi-control-proto`.
