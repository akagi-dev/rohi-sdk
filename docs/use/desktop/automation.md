# Automation

Every `rohi` command can run unattended: results go to stdout, prompts and diagnostics to stderr, `-o json` selects a versioned document, exit codes are stable and confirmations can be skipped. Combine these with the device selection of [Discovery & connection](discovery.md).


Command results go to stdout. Prompts, progress, warnings and diagnostics go to stderr, so redirects and pipes stay
clean. `-o json` (`--output json`) prints a JSON document with a stable, versioned schema instead of text:

```bash
rohi -D A12F -o json status network | jq -r '.values.ip'
rohi -D A12F -o json status sensors | jq '.values["scd4x.co2"]'
rohi -D A12F -o json status | jq -r '.services[] | select(.health != "ok") | .service'
rohi -D A12F -o json config wifi | jq '.overridden'   # {"section", "values", "overridden": [field names]}
```

Every JSON document has `"schema_version": 1`. The schemas are owned by the command line, not by the library types.
A closed pipe (`rohi action | head -1`) ends the command quietly.

| Exit code | Meaning |
|-----------|---------|
| `0` | success |
| `1` | failure of the device, the link or the command (including a declined prompt) |
| `2` | command line usage error |
| `130` | interrupted with Ctrl-C |

Failures print an `error:` line and, where there is something to try, `hint:` lines:

```text
$ rohi status
error: no ROHI device found

hint: make sure Bluetooth is enabled and the device is advertising
hint: try `rohi scan`
```

Destructive commands (`config reset`, `config apply`) ask for confirmation on a terminal; the actions of the device (`system factory_reset`) do not. Without a
terminal they fail with `confirmation required in non-interactive mode` unless `--yes` is passed. Secrets are never
accepted as command line arguments: a field or argument that is a secret is read with `NAME=-` from stdin, or from a
hidden prompt on a terminal.

