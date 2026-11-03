# Service pipeline (`rohi-service`)

`rohi-service` is the one place where a firmware module describes itself to the outside world. A module offers up to
three things, all through generic types, so the [control protocol](control.md), the `rohi` tool and any other client
never know a module by name:

| Plane | Question | Owner type |
|-------|----------|------------|
| **Config** | what is the device *set up* to do? (desired, persistent) | `Config` inside a `Section` |
| **Status** | what is happening *now*? (observed, read-only) | `Status` |
| **Actions** | what can I *ask it to do*? (explicit operations) | `Action` |

The three are separate on purpose: a service can exist without a config section, a section can exist without a
service, and an action is never smuggled into status. A module describes its fields once, as constants, and converts
between a generic `Value` and its typed Rust fields; patching, secret redaction, status bookkeeping (state, health,
issues, revision) and discovery are generic. Config is the *effective* configuration (module defaults, firmware
overrides, user overrides) with atomic patches validated as a whole, see [Configuration store](config.md). Registries
are tuples of sections or services that dispatch by stable numeric id: adding a feature means adding it to the tuple,
not changing the protocol or the client. The user-facing view is in [Status & diagnostics](../use/desktop/status.md),
the steps to add one are in [Add a service](../guides/add-service.md), and the ideas in the [build guide](../build/status.md).

The crate is `no_std`, allocation free and portable; it depends neither on the control protocol nor on feature crates.
Each feature crate implements the traits for its own types and the firmware builds the registries over its runtime
handles for the dispatcher. Its limits mirror the protocol limits (see
[Resource limits](../reference/resource-limits.md#control-protocol)); names, ids and field ids are wire and storage ABI
([Persistent ABI](../reference/persistent-abi.md)).

| Item | Role |
|------|------|
| `Config`, `Section`, `SectionStore`, `ConfigRegistry` | typed effective config, generic patch/reset/read behaviour, firmware-provided persistence, dispatch by section id |
| `Status`, `Statuses`, `StatusRegistry` | live state, health, readiness, issues and values of a service; revision and state age bookkeeping |
| `Action`, `ActionRegistry` | explicit operations with typed arguments and result records |
| `Service`, `#[derive(Service)]` | stable name and id of a service (`rohi-service-derive`) |
| `Value`, `FieldDescriptor`, `ConfigField` | the generic value and the const field descriptions |

## Reference

- docs.rs: [`rohi-service`](https://docs.rs/rohi-service), [`rohi-service-derive`](https://docs.rs/rohi-service-derive);
  SDK level: [`rohi-sdk`](https://docs.rs/rohi-sdk)
- Source: [service](https://github.com/akagi-dev/rohi-sdk/tree/master/crates/service),
  [service-derive](https://github.com/akagi-dev/rohi-sdk/tree/master/crates/service-derive)
- Local build: `cargo doc --no-deps --open -p rohi-service` (also `-p rohi-service-derive`)
