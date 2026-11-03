# Actions

Actions are the operations a device offers: scan for Wi-Fi networks, reconnect, provision an identity. The device lists them with typed arguments and results; `rohi` generates its help and tables from that description. How a firmware declares an action is in [Action](../../build/action.md).


```bash
rohi action                                        # all actions: service, name, privileged
rohi action network                                # the actions of one service with their arguments
rohi action network scan --help                    # arguments and results (generated from the device)
rohi action network scan                           # results as a table, one row per record
rohi action network reconnect
rohi action radio tune channel=11                  # arguments are NAME=VALUE, all required
rohi -o json action network scan
```

`-o json` with `--help` prints the schema. Privileged actions need a link that satisfies the authorization policy of
the device. A refusal with a stable reason is reported as `the device rejected the action: <reason>` (for example
`already_provisioned`).

## Identity

The identity is an ordinary service: no dedicated command.

```bash
rohi status identity                                   # state, public key; never the secret
rohi action identity provision secret=-               # 32-byte secret as hex, from stdin or a hidden prompt
rohi action identity erase
```

The secret is derived on a trusted machine (`rohi::derive_identity`, see [Device identity](https://docs.rs/rohi-identity)) and
only the derived secret is sent. `rohi factory-reset` erases it with the rest of the configuration storage. A malformed secret is reported
without echoing it.

