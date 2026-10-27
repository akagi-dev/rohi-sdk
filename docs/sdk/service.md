# Service pipeline (`rohi-service`)

`rohi-service` is the one place where a firmware module describes itself to the outside world. A module offers up to
three things, all through generic types, so the control protocol, the `rohi` tool and any other client never know a
module by name:

| Plane | Question | Owner type | Wire operations |
|-------|----------|------------|-----------------|
| **Config** | what is the device *set up* to do? (desired, persistent) | [`ManagedConfig`](#config-sections) | `ListConfigSections`, `GetConfigSchema`, `GetConfigSection`, `PatchConfigSection`, `ResetConfigSection` |
| **Status** | what is happening *now*? (observed, read-only) | [`ManagedService`](#status) | `GetServicesStatus`, `GetServiceSchema`, `GetServiceStatus` |
| **Actions** | what can I *ask it to do*? (explicit operations) | [`ManagedService::ACTIONS`](#actions) | `ListActions`, `GetActionSchema`, `InvokeAction` |

Capabilities, config and status are separate on purpose: a service can exist without a config section, a section
can exist without a service, and an action is never smuggled into status. See the user-facing view in
[Services and status](../getting-started/services.md).

```text
 rohi-control (dispatcher)  ──►  ControlBackend { config(), services() }
                                       │                    │
                          ConfigRegistry (tuple)     ServiceRegistry (tuple)
                                 │                          │
                   ManagedSection<C, Store>          Tracked<S: ManagedService>
                                 │                          │
                     typed section C: ManagedConfig   live runtime state of S
```

`rohi-service` is `no_std`, allocation free and portable. It does not depend on the protocol, on `rohi-config` or on
any feature crate; each feature crate implements the traits for its own types (`rohi-net`, `rohi-connectivity`,
`rohi-time`, `rohi-hal`, `rohi-identity`, `rohi-location`).

## Values and fields

Everything crossing the boundary is a `Value`: `Bool`, `I64`, `U64`, `F32`, `String`, `Bytes` or `Enum`. Floats are
single precision; a counter, a timestamp or an id is an integer. An `Enum` value is a **number** (`u32`). A
`FieldDescriptor { id, name, kind, unit, enum_id }` describes one field once, as a `const`:

```rust,ignore
FieldDescriptor::new(2, "rssi", ValueKind::I64).unit(Unit::Dbm)
```

The `Unit` is a code, not text, and there is no description: documentation belongs to these pages, not to every
response of the firmware. The named variants of an enum field are listed once per section, service or action as
`EnumDescriptor::new(id, &[EnumVariant::new(value, "name"), ..])` (`ManagedConfig::ENUMS`, `ManagedService::ENUMS`,
`ActionDescriptor::enums`) and the field refers to the table with `.with_enum(id)`; checked values must be one of its
variants.

Field **ids are stable and only ever added**; names use dotted paths for nested fields (`bmx280.temperature`). Config
fields extend the descriptor (`ConfigField`) with `writable`, `secret`, `optional` and typed `Bounds` (`Bounds::U64(min,
max)`, `I64`, `F32`): the bounds are checked by the device with the integers staying integers and are not sent to
clients; the typed `validate()` stays authoritative. `Value`'s
`Debug` shows only its kind, so a secret never reaches a log by accident.

## Config sections

A module implements `ManagedConfig` for its **typed** section and keeps all knowledge about it:

```rust,ignore
impl ManagedConfig for ConnectivityConfig {
    const SECTION_ID: u64 = <Self as ConfigSection>::ID.0; // the stored section id, the address of requests
    const NAME: &'static str = "connectivity";
    const VERSION: u16 = 1;
    const FIELDS: &'static [ConfigField] = &[
        ConfigField::new(3, "interval_s", ValueKind::U64).unit(Unit::S).bounds(Bounds::U64(10, 86_400)),
        // ...
    ];
    fn defaults() -> Self { /* base of a patch when nothing is stored */ }
    fn read_field(&self, id: u32) -> Result<Option<Value>, FieldError> { /* typed -> Value */ }
    fn field_is_set(&self, id: u32) -> Result<bool, FieldError> { /* without reading a secret */ }
    fn set_field(&mut self, id: u32, value: Value) -> Result<(), FieldError> { /* Value -> typed */ }
    fn unset_field(&mut self, id: u32) -> Result<(), FieldError> { /* optional fields */ }
    fn validate(&self) -> Result<(), FieldError> { /* the whole section, across fields */ }
}
```

The schema only expresses simple constraints (kind, enum variants, numeric range); everything else stays in typed
`validate`. A `ManagedSection<C, S, M>` adds the generic behaviour on top of a `SectionStore<C>` (`load`, `store`,
`remove`, `restart_required`), which the firmware implements over its configuration owner task:

1. **Patch semantics.** A patch is a list of `set` / `unset` mutations by field id. Load the stored section (or
   `defaults()`), apply all mutations to a copy, validate the **complete** result, store atomically, apply the runtime
   effect, increment the revision. If anything is invalid, nothing is stored. A field that is not mentioned keeps its
   value; `unset` is distinct from an empty string or zero.
2. **Secrets** (`secret`) are write-only: `GetConfigSection` reports them as `redacted`, never their value.
3. **Dry run** validates exactly like a real patch and stores nothing; the revision does not change.
4. **Revision** (`u32`, RAM only, 0 at boot) increments on every successful write or reset; `expected_revision`
   makes a patch fail with `CONFLICT` if the section changed meanwhile.
5. **Reset** removes the stored section and restores the module defaults.
6. `restart_required` tells the client that a change needs a restart.

The store must commit **and** publish to the runtime in one step, so a cancelled caller can never leave a stored value
unknown to the runtime ([Durable mutation](../concepts/async-and-cancellation.md#durable-mutation)).

A `ConfigRegistry` is a **tuple of sections**; it dispatches by name:

```rust,ignore
type ConfigSections = (
    ManagedSection<WifiConfig, Stored<WifiConfig>, RawMutex>,
    ManagedSection<ConnectivityConfig, Stored<ConnectivityConfig>, RawMutex>,
    ManagedSection<LocationConfig, Stored<LocationConfig>, RawMutex>,
);
```

Adding a section means adding it to the tuple (up to 8). Nothing else changes: not the protocol, not the client, not
the CLI.

## Status

A module implements `ManagedService` on top of its **live** state. There is no stored status struct to duplicate:

```rust,ignore
impl ManagedService for WifiService<'_, M, D> {
    const ID: u32 = 1;                              // stable numeric id, the address of requests
    const NAME: &'static str = "wifi";
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[ /* ssid, rssi, ip, ... */ ];
    async fn summary(&self) -> ServiceSummary { /* state, health, ready */ }
    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) { /* active issues */ }
    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> { /* fields */ }
}
```

State, health and readiness are **separate** values, never one big enum:

| | Values |
|---|---|
| `ServiceState` (lifecycle) | `disabled`, `stopped`, `starting`, `running`, `stopping`, `failed` |
| `ServiceHealth` (quality) | `unknown`, `ok`, `degraded`, `error` |
| `ready` | whether the service can currently do its job |

Examples: a connecting Wi-Fi is `starting / unknown / not ready`; a telemetry service that cannot reach its backend is
`running / degraded / not ready`; an optional GPS turned off is `disabled / unknown / not ready`, which is **not an
error**.

**Issues** are the currently active problems, each with a stable numeric `code`, a severity (`info`, `warning`,
`error`, `critical`) and an age. The names of the codes are listed once in `ManagedService::ISSUES` and sent with the
schema, never with every status; there is no message text. Codes below 256 are reserved for the SDK (`Startup` reports
`ISSUE_STARTUP_FAILED`), services start at `FIRST_SERVICE_ISSUE`. History does not belong here.

`Tracked<S, M>` wraps a service and adds what every service needs without its cooperation: a **status revision**
that changes when state, health, readiness or the active issues change (values that vary all the time, such as
counters or signal strength, deliberately do not bump it), and the **time in the current state**. Revisions and ages
are derived from what the service reports when a client asks.

**Device health** is decided by the firmware, never by the client. `ControlBackend::device_health` defaults to
`default_device_health`: disabled services do not count; a failed or `error` service is an error for the device when
it is `ESSENTIAL` (the default) and a degradation otherwise; a `degraded` service, or a running one that is not
ready, degrades the device. Firmware with other redundancy rules overrides it.

## Actions

Status is read-only. Everything that *does* something is an action declared by the service:

```rust,ignore
const ACTIONS: &'static [ActionDescriptor] = &[
    ActionDescriptor::new(1, "scan")
        .results(&[ /* ssid, rssi, channel, security */ ]),
    ActionDescriptor::new(2, "reconnect").privileged(),
];

async fn invoke(&self, action: u32, args: &[(u32, Value)], out: &mut impl ActionOutput)
    -> Result<(), ActionError> { /* ... */ }
```

An action takes typed **arguments** (all required, checked against the descriptor before `invoke` runs) and returns
zero or more **records** of typed fields (a scan returns one record per network, a reconnect none). `privileged()`
requires the write privilege of the link policy; `secret()` marks arguments that are secrets (identity provisioning):
they imply `privileged`, are the `Provision` privilege, and are overwritten after the call. An action can fail with
`InvalidArgument`, `Busy`, `Unavailable`, `Internal` or `Rejected("code")`, a refusal with a stable machine-readable
reason such as `already_provisioned`.

## Registries

`ServiceRegistry` is a tuple of `Tracked` services, like the config registry. Both traits are implemented for `()`
(a device with no sections or services) and for tuples of up to 8. Requests address sections, services and actions
by their numeric ids, which must be unique within a registry: `Device::new` panics in every build, including release,
when two sections or two services share an id. Names are for discovery.

Schemas are validated when the section or service is built, as a compile-time error: section, service, action, field
and issue names are not empty and at most 24 bytes (variant names 16), so nothing is ever truncated on the wire; field
ids and names are unique within a schema, action ids and names within a service, issue codes and names within a service
(codes from `FIRST_SERVICE_ISSUE`), enum ids are non-zero and unique, and variant values and names are unique within
their enum. An enum field must refer to an enum of its table, no other field may.
A section, service or action has at most one enum (`MAX_ENUMS`), a constraint of the compact protocol.

## What lives where

| Crate | Config section | Service | Actions |
|-------|----------------|---------|---------|
| `rohi-net` | `wifi` (`ssid`, `password` secret, timeouts, backoff), `network` (address mode, static IPv4, host name, DHCP timeout), `ble` (`name`) | `wifi` (link, radio heap), `network` (address, sockets), `ble` (state, name, link security) | `wifi`: `scan`, `connect`, `reconnect`, `disconnect` |
| `rohi-connectivity` | `connectivity` | `connectivity` (queue, counters, endpoint) | |
| `rohi-location` | `location` | `location` | |
| `rohi-time` / `rohi-time-ntp` | `time` (NTP servers, `rohi-time-ntp`) | `time` (unix time, source, age, failures) | |
| `rohi-hal` | | `sensors` (`<sensor>.<measurement>` values) | |
| `rohi-identity` | | `identity` (provisioned, public key) | `provision`, `erase` |

The firmware (`altruist-urban/src/control.rs`) builds the two tuples over its runtime handles and gives them to the
dispatcher. [Add a service](../guides/add-service.md) shows the steps.

## Limits

Bounds keep every message small enough for a microcontroller stack; they are defined once in
`rohi-control-proto/build.rs` and listed in [Control plane](control.md#limits). In memory: `MAX_FIELDS` (status
fields per service) 20, `MAX_CONFIG_FIELDS` 16, `MAX_MUTATIONS` 8, `MAX_ISSUES` 4, `MAX_ACTION_ARGS` 3, `MAX_RECORDS` 6,
`MAX_RECORD_FIELDS` 4, string and bytes values 64 bytes.
