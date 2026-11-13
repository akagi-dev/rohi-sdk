# Actions

Actions are the operations a device offers: scan for Wi-Fi networks, reconnect, provision an identity. The device lists them with typed arguments and results; `rohi` generates its help and tables from that description. How a firmware declares an action is in [Action](../../build/action.md).


```bash
rohi action                                        # all actions: service, name, access
rohi action network                                # the actions of one service with their arguments
rohi action network scan --help                    # arguments and results (generated from the device)
rohi action network scan                           # results as a table, one row per record
rohi action network reconnect
rohi action radio tune channel=11                  # arguments are NAME=VALUE, all required
rohi -o json action network scan
```

`-o json` with `--help` prints the schema. Every action declares the access it needs: `read`, `mutate` or
`provision`; the link must satisfy that under the authorization policy of the device. Access is independent of whether
the arguments are secrets. A refusal with a stable reason is reported as `the device rejected the action: <reason>` (for example
`already_provisioned`).

When an action returns a stream, its file bytes are written to stdout in the order reported by the device; file names and sizes
are written to stderr. Stream output is raw bytes, not JSON.

## Without the word `action`

Every action also runs as `rohi SERVICE ACTION [ARG]...`, with positional arguments. The positions are those of the
arguments in the schema the device reports (see `rohi SERVICE ACTION --help`), never their numeric ids:

```bash
rohi system                                        # the actions of a service, from the device
rohi system reboot --help                          # arguments, kinds, enum variants and the access level
rohi system reboot                                 # reboots the device shortly after answering
rohi system factory_reset                          # formats the configuration storage; does not reboot
rohi wifi scan
rohi -o json system factory_reset                  # global options may come before or after the service
```

For an action whose schema lists `endpoint` (string), `enabled` (bool) and `timeout_ms` (u64) in that order, the
words are given in that order, whatever the numeric ids of the fields are:
`rohi SERVICE ACTION https://example.org true 5000`. Put `--` before a value that starts with `-`
(`rohi SERVICE ACTION -- --odd`); negative numbers need no `--`.

All arguments are required: a missing or extra word is refused before the device is contacted for the call. Values are
checked against their kind, an enum value must be one of the listed variants. A secret argument is read with `-`
(hidden prompt or stdin), never from the command line: `rohi identity provision -`. A service whose name is also a
command of `rohi` (`status`, `config`, ...) is reached with `rohi action SERVICE ...`. The listing and the help need the
device, because they come from it.

## Identity

The identity is an ordinary service: no dedicated command.

```bash
rohi status identity                                   # state, public key; never the secret
rohi action identity provision secret=-               # 32-byte secret as hex, from stdin or a hidden prompt (or: rohi identity provision -)
rohi action identity erase
```

The secret is derived on a trusted machine (`rohi::derive_identity`, see [Device identity](https://docs.rs/rohi-identity)) and
only the derived secret is sent. `rohi system factory_reset` erases it with the rest of the configuration storage. A malformed secret is reported
without echoing it.

