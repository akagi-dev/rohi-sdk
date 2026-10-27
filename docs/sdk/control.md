# Control plane (BLE)

The control plane lets a desktop (or any BLE central) inspect and configure a ROHI device locally. It is **generic**:
the protocol, the dispatcher and the client know no module by name. They offer four planes, all driven by schemas the
device publishes: device **info**, persistent **config**, runtime **status** and explicit **actions**. What those
planes contain (Wi-Fi, telemetry, sensors, identity, location, ...) is decided by the firmware's registries, see
[Service pipeline](service.md). It is **local control only**: uplink telemetry, HTTP and SoftAP are out of scope.

```text
 BLE radio
     │
 rohi-net-esp::ble         ESP32 backend (esp-radio HCI + TrouBLE GATT)       platform
     │  BleTransport
 rohi-control-ble          Control Service, framing, fragmentation            portable
     │  bytes
 rohi-control              dispatcher + authorization policy                  portable
     │  rohi-control-proto messages (bounded protobuf)
 ControlBackend            device info, uptime, config registry, service registry
     ├── ConfigRegistry    tuple of ManagedSection<..>     (rohi-service)
     └── ServiceRegistry   tuple of Tracked<ManagedService> (rohi-service)
              │
        feature crates implement ManagedConfig / ManagedService over their runtime handles

 rohi (library + CLI)      desktop client, btleplug
```

Dependency direction: `rohi-control-proto ← rohi-control ← rohi-control-ble → rohi-net ← rohi-net-esp ← altruist-urban`
(see [Crate map](../concepts/crate-map.md)). `rohi-control` depends only on the protocol and `rohi-service`: it has no
dependency on Wi-Fi, telemetry, sensors, time or identity, and no ESP specific control crate exists: the ESP part only
exposes a generic byte pipe. `rohi-net*` never depends on `rohi-control*`.

## Crates

| Crate | Role |
|-------|------|
| `rohi-net` (`ble`) | `BleTransport`, `BleState`, `BleConnectionInfo`, `BleConfig`, the `ble` section (`BleSettings`) and service (`BleService`): portable BLE API, no vendor types |
| `rohi-net-esp` | `EspWireless` (shared radio runtime), `EspWifi`, `EspIp`, `EspBle` |
| `rohi-control-proto` | `control.proto`, bounded generated types (`micropb`), `PROTOCOL_VERSION`, `limits` |
| `rohi-service` | the generic model: `Value`, `ManagedConfig`, `ManagedService`, registries; see [Service pipeline](service.md) |
| `rohi-control` | `Controller`, `ControlBackend`, `Device` (a backend built from registries), `AuthorizationPolicy` |
| `rohi-control-ble` | Service/characteristic UUIDs, `frame` (fragmenter, reassembler), `ControlBle` control loop |
| `rohi` (`tools/rohi`) | Desktop library (`Device`, `Session`, `BleLink`, model) and the `rohi` command line tool |

Everything except `rohi-net-esp`, the firmware and `tools/rohi` builds and is tested on the host.

## Protocol

One protobuf request, one protobuf response, correlated by `request_id`. The schema is
`crates/rohi-control-proto/proto/rohi/control/v1/control.proto`. `protocol_version` is `1` in every message; the
protocol is still under development and changes without a version bump, so keep the tool and the firmware on the
same revision. Unknown requests, a wrong version or an unknown name return a typed error; the link is never dropped
because of a bad request.

| Plane | Request | Needs a secure link policy |
|-------|---------|----------------------------|
| info | `GetInfo` | no |
| config | `ListConfigSections`, `GetConfigSchema`, `GetConfigSection` | no |
| config | `PatchConfigSection`, `ResetConfigSection` | **yes** (`Privilege::Mutate`) |
| config | `FactoryReset` | **yes** (`Privilege::Provision`, it erases the identity) |
| device | `Reboot` | **yes** (`Privilege::Mutate`) |
| status | `ListServices`, `GetServicesStatus`, `GetServiceSchema`, `GetServiceStatus` | no |
| actions | `ListActions`, `GetActionSchema` | no |
| actions | `InvokeAction` | depends on the action: open, `privileged` (**Mutate**) or secret arguments (**Provision**) |

Sections, services and actions are addressed by **name** (a string). There is no capability list of modules: whether
something exists is answered by the `List...` requests. Errors:

| Error | Meaning |
|-------|---------|
| `INVALID_ARGUMENT` | a value, field or argument is missing, unknown, of the wrong kind, out of range or fails the typed validation; nothing was changed |
| `NOT_FOUND` | no such section, service or action |
| `NOT_SUPPORTED` | unknown request or wrong protocol version |
| `BUSY`, `UNAVAILABLE` | try again later; the backing service or storage is not available |
| `PERMISSION_DENIED` | the link does not satisfy the authorization policy |
| `CONFLICT` | `expected_revision` no longer matches the section |
| `REJECTED` | an action ran and was refused for the stable reason in `Error.reason` (for example `already_provisioned`) |
| `INTERNAL` | unexpected device failure |

A response with `request_id = 0` means the device could not parse the request at all.

### Values and schemas

Config values, status values, action arguments and results all use one `Value` (`bool`, `i64`, `u64`, `f64`,
`string`, `bytes`, `enum`) and one `FieldDescriptor { id, name, kind, description, unit, variants }`. The client
fetches a schema on demand and parses and renders values by it; ids are stable and only ever added. **Schemas are paged**
(four fields per response, `offset` in the request, `total` in the response) so a response stays small on a
microcontroller; the `rohi` library follows the pages and caches the result for the connection.

### Config

A section has a name, a schema version and a **revision**. `GetConfigSection` returns the fields that are set (unset
fields are absent, secrets are `redacted`, never their value) and `configured`, whether anything is stored.
`PatchConfigSection` carries `set` / `unset` mutations by field id, an optional `expected_revision` and `dry_run`; it is
atomic per section and validated as a whole, see [Service pipeline](service.md#config-sections).
`ResetConfigSection` removes the stored section. The response carries the new `revision` and `restart_required`.
`FactoryReset` formats the configuration storage: every section, sections the firmware does not know and the identity are
removed and the runtime returns to the defaults. The response carries `restart_required`.
`Reboot` asks the device to reboot: it answers first and reboots shortly after (the Altruist Urban about 1.5 s later), so
the link is lost. A device without a reboot hook answers `NOT_SUPPORTED`.
Success means the configuration is stored **and handed to its runtime**, not that the network was reached: observe the
service status. There is no request that touches several sections; a client that needs that applies them one by one.

### Status

`GetServicesStatus` is the compact request: the device uptime, the device health **decided by the firmware** and one
summary per service (`state`, `health`, `ready`, `revision`, `state_age_ms`, `reason`). It never carries
service-specific values. `GetServiceStatus` returns the summary, one page of the service's values and the currently
active issues (stable `code`, severity, message, age). Status is read-only: there is no start, stop or reset in it.
Pollers compare `revision`s and fetch the detail only when one changed. See [Services and
status](../getting-started/services.md).

### Actions

`ListActions` returns `(service, name, description, privileged, secret_args)` in pages; `GetActionSchema` returns the argument
and result fields. `InvokeAction` takes all arguments (typed, all required) and returns up to `MAX_RECORDS` records
of typed fields. Arguments that are secrets are overwritten after the call; neither they nor the status of a service
ever contain a secret.

## BLE binding

One GATT service with two characteristics:

| | UUID | Direction |
|---|------|-----------|
| ROHI Control Service | `726f6869-0001-4d00-8000-636f6e74726f` | |
| RX | `726f6869-0002-4d00-8000-636f6e74726f` | central writes a chunk (write / write without response) |
| TX | `726f6869-0003-4d00-8000-636f6e74726f` | peripheral notifies a chunk |

The device advertises the service UUID and the name `ALTRUIST-URBAN-XXXX` (scan response), where `XXXX` are the last two bytes of the device MAC in hex.

A message is split into frames of at most one ATT payload (MTU − 3, between 20 and 244 bytes). The framing does not
know protobuf:

```text
| version u8 | msg id u8 | index u8 | count u8 | total len u16 LE | payload chunk |
```

Frames arrive strictly in order; a new first fragment (`index 0`) starts a new message. `count` and `total len` must
not change within a message. Bad frames (bad version, invalid or duplicate index, a skipped fragment, a changed
header, an oversized message, a length mismatch) are rejected: the partial message is dropped, the rest of that
message is skipped, and the device answers once with `INVALID_ARGUMENT` (`request_id 0`). Disconnecting drops all
partial state; a response is never sent to a different connection than the one that asked.

One connection, one request in flight, one response. Nothing runs in a radio callback: GATT writes are copied into a
bounded queue (the ATT response waits for room, which is the backpressure), the control task reassembles and
dispatches, and responses leave through a bounded TX queue.

### Limits

| Constant | Value |
|----------|-------|
| `MAX_REQUEST_LEN` | 768 B (a patch of 8 full values is the worst case) |
| `MAX_RESPONSE_LEN` | 2048 B |
| ATT payload | 20 – 244 B, header 6 B, at most 255 fragments |
| `MAX_NAME_LEN` (section, service, field, action) | 24 B |
| `MAX_VALUE_LEN` (string and bytes values) | 64 B |
| `MAX_DESCRIPTION_LEN` / `MAX_UNIT_LEN` | 40 B / 8 B |
| `MAX_VARIANTS` / `MAX_VARIANT_LEN` | 10 / 16 B |
| `MAX_SCHEMA_PAGE` / `MAX_STATUS_PAGE` / `MAX_ACTIONS_PAGE` | 4 / 8 / 8 entries per response |
| `MAX_CONFIG_FIELDS` | 16 fields per section |
| `MAX_MUTATIONS` | 8 per patch |
| `MAX_CONFIG_SECTIONS` / `MAX_SERVICES` | 8 / 8 |
| `MAX_ISSUES` / `MAX_CODE_LEN` / `MAX_ISSUE_MESSAGE_LEN` | 4 / 32 B / 48 B |
| `MAX_ACTION_ARGS` / `MAX_RECORDS` / `MAX_RECORD_FIELDS` | 3 / 6 / 4 |

The limits are defined once (`rohi-control-proto/build.rs`), applied to the generated types and checked by tests:
requests must fit `MAX_REQUEST_LEN` in the worst case, full schema, status and action responses must encode within
`MAX_RESPONSE_LEN`, and a **memory budget test** keeps the largest request or response variant under about 2.7 KB on the
host. That last one matters on the device: responses are built and moved on the stack of the control task, and the
main stack of the ESP32-C3 is only about 42 KB. The BLE queue depths belong to the product and are in
[Resource limits](../reference/resource-limits.md#ble).

### Security

`rohi-net` reports `encrypted` / `authenticated` for the current link. `rohi-control` turns that into decisions
with an `AuthorizationPolicy` (`Privilege::Read`, `Mutate`, `Provision`): `OpenPolicy`, `RequireEncryption`,
`RequireAuthenticated`. Config writes and `privileged` actions need `Mutate`; actions with secret arguments (identity
provisioning) need `Provision`, so a stricter policy can protect them through the same hook. The policy runs
**before** the action does. The Altruist Urban firmware currently runs `OpenPolicy`, and the BLE backend does not pair
yet (TrouBLE's `security` feature is not enabled), so every link reports unencrypted. Anyone in radio range can use the
control plane until a policy is configured, **including identity provisioning and erase**. Secrets such as the Wi-Fi
password still cross the air in clear text until pairing is enabled.

## `rohi` desktop client

`tools/rohi` is one package: the **library** `rohi` carries the device protocol, the **binary** `rohi` is a thin
command line layer.

```text
rohi library
  Device         generic async API: info,
                 config_sections, config_schema, config_get, config_patch, config_reset,
                 services, services_status, service_schema, service_status,
                 actions, action_schema, invoke        (schemas fetched on demand and cached)
  Session        framing, request ids, timeouts, typed error mapping   (reuses rohi-control-ble::frame)
  ClientTransport  chunk link trait; BleLink (btleplug) implements it, tests use an in-memory one
  model          plain Rust types (no protobuf), optional serde
  format         human readable rendering driven by the schemas
```

No method is named after a module. The command line has one authoritative page, [The rohi tool](../getting-started/rohi-tool.md):
commands, device selection (`--device`, `ROHI_DEVICE`, `ble://` and `tcp://` URIs), the output contract (results on
stdout, prompts and diagnostics on stderr, `-o json`, exit codes), configuration files and actions. It is a layer over
`Device`: parsing, device choice, prompts and output live in `tools/rohi/src/cli`, protocol behaviour stays in `Device`
and below, and the command dispatch (`rohi::cli::execute`) is generic over `ClientTransport`, so it runs unchanged
against the in-memory device of the tests, which is built from the real `rohi-control`, `rohi-service` and feature
crates.

Secrets are never printed, exported or accepted on a command line. On Linux the build needs `libdbus` and `pkg-config`
(provided by `nix develop`); the protobuf compiler `protoc` is needed to build `rohi-control-proto`.

## Build, test and measurements

Commands are listed once in [Build and test](../getting-started/build-and-test.md) and
[Targets and features](../reference/targets-and-features.md). The control plane is covered by host tests (protocol,
dispatcher, framing, `rohi` client end to end); flash, RAM and heap measurements are in
[Resource budget](../validation/resource-budget.md) and the open hardware checks in
[Hardware testing](../validation/hardware-testing.md). The shared radio runtime that serves BLE and Wi-Fi is described in
[Wireless runtime](../platform/wireless.md).
