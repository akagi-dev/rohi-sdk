# Action

An **action** is an operation a user can ask the device to perform: scan for networks, reconnect, provision an identity.
Actions are declared by a service, discoverable, and have typed arguments and results.

- Arguments are typed and all required; the SDK checks them against the declaration before the service runs.
- Results are records of typed fields: a scan returns one record per network, a reconnect none.
- An action that changes anything is **privileged**: the link's authorization policy must grant write access. An action
  with secret arguments never logs, returns or keeps them.
- A refusal that a client may react to carries a stable code, for example `already_provisioned`.

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
| Declarations, privileges, errors | [`rohi-service` rustdoc](https://docs.rs/rohi-service) |
