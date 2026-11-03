# Action

An **action** is an operation a user can ask the device to perform: scan for networks, reconnect, provision an identity.
Actions are declared by a service, discoverable, and have typed arguments and results.

## A small example

```rust,ignore
const ACTIONS: &'static [ActionDescriptor] = &[
    ActionDescriptor::new(1, "scan")
        .results(&[ /* ssid, rssi, channel, security */ ]),
    ActionDescriptor::new(2, "reconnect").privileged(),
];

async fn invoke(&self, action: u32, args: &[(u32, Value)], out: &mut impl ActionOutput)
    -> Result<(), ActionError> { /* ... */ }
```

- Arguments are typed and all required; the SDK checks them against the descriptor before `invoke` runs.
- Results are records of typed fields: a scan returns one record per network, a reconnect none.
- `privileged()` asks the link's authorization policy for write access; `secret()` marks arguments that must never be
  logged, returned or kept.
- A refusal that a client may react to is a stable code, for example `Rejected("already_provisioned")`.

With the service registered, every client lists the action and builds its arguments and output from the schema:

```bash
rohi action                      # all actions
rohi action wifi scan --help     # arguments and results, generated from the device
rohi action wifi scan
```

The user's view is [Actions](../use/desktop/actions.md).

## Go deeper

| Topic | Page |
|-------|------|
| Step by step | [Add a control operation or an action](../guides/add-control-operation.md) |
| Descriptor, privileges, errors | [Service pipeline](../sdk/service.md) |
