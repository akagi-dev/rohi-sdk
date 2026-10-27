# Services and status

A ROHI device is a set of **services**: Wi-Fi, telemetry, time, sensors, identity, location, and whatever a firmware
adds. This page explains how to see what each one is doing and how that differs from configuring it, using the
[`rohi` tool](rohi-tool.md). Nothing here is specific to a module: the device describes its services, sections and
actions, and the tool shows them.

## Four questions, four places

```text
Capability    what can this firmware do?          (kept for runtime operations, e.g. firmware update)
Config        what is it set up to do?            persistent, desired state      rohi config ...
Status        what is it actually doing now?      live, observed state, read-only rohi status ...
Actions       what can I ask it to do?            explicit operations            rohi action ...
Diagnostics   what happened before?               logs, history                  (separate concern)
```

The same service appears in several places, never mixed:

```console
$ rohi config get connectivity      # desired: what it is set up to do
enabled     true
node_id     42
interval_s  60 s
host        ingress.example.org
port        443

$ rohi status connectivity          # observed: what is happening now
Service:  connectivity
State:    running
Health:   degraded
Ready:    yes
Since:    34s

frames_created           212
frames_delivered         209
send_retries             7
frames_rejected          0
queue_depth              3
endpoint                 ingress.example.org:443
frames_without_location  212

Issues:
  WARNING  no_location  (34s)
           No location is set; telemetry is sent without it
```

## The device summary

`rohi status` is **one compact request**: the device health and one row per service. It stays small however many
fields a service has.

```console
$ rohi status
Device:  altruist-urban-0.4.0 (urban-esp32c3)
Uptime:  2h 14m
Health:  degraded

SERVICE       STATE     HEALTH    READY  SINCE
network        running   ok        yes    2h 14m
connectivity  running   degraded  yes    34s
time          running   ok        yes    2h 13m
sensors       running   ok        yes    2h 14m
identity      running   ok        yes    2h 14m
location      disabled  -         no     -
```

**State, health and readiness are three separate things**, not one big status:

| | Answers | Values |
|---|---|---|
| State | where is it in its lifecycle? | `disabled`, `stopped`, `starting`, `running`, `stopping`, `failed` |
| Health | how well does it work? | `ok`, `degraded`, `error`, or `-` (unknown) |
| Ready | can it do its job *right now*? | `yes` / `no` |

So the same word means different things in different columns, and combinations carry information:

| State | Health | Ready | Meaning |
|-------|--------|-------|---------|
| `running` | `ok` | yes | working |
| `running` | `degraded` | yes | works, but something is wrong (telemetry without a location) |
| `running` | `degraded` | no | the task runs, the backend does not answer |
| `starting` | `-` | no | Wi-Fi is still connecting |
| `disabled` | `-` | no | turned off by configuration or never set: **not an error** |
| `failed` | `error` | no | crashed and not recovering |

`SINCE` is how long the service has been in its current state.

### Device health

`Health:` in the header is decided **by the firmware**, not computed by the tool. The firmware knows which services
are mandatory, optional or redundant: a cellular modem may be degraded while Ethernet is fine. With the default policy
a failed essential service makes the device `error`, a degraded service or a running service that is not ready makes
it `degraded`, and disabled services do not count.

## One service in detail

`rohi status <service>` adds the service-specific values the device describes (with their units) and the **active
issues**:

```console
$ rohi status network
Service:  network
State:    running
Health:   ok
Ready:    yes
Since:    2h 14m

ssid     office
rssi     -61 dBm
ip       192.168.1.42
gateway  192.168.1.1
dns      192.168.1.1
```

An **issue** has a stable machine-readable code (`connection_timeout`, `no_location`), a severity, a message and an
age. Scripts react to the code, people read the message. Only issues that are active *now* are listed; history is
diagnostics. With `-o json` the values are keyed by field name and carry their units:

```console
$ rohi -o json status network
{
  "schema_version": 1,
  "service": { "service": "network", "state": "running", "health": "ok", "ready": true, ... },
  "values": { "ssid": "office", "rssi": -61, "ip": "192.168.1.42" },
  "units": { "rssi": "dBm" },
  "issues": []
}
```

## Watching

```console
$ rohi status --watch                 # reprint when something observable changed
$ rohi status connectivity --watch --interval 5
```

Every service has a **status revision** that changes when its state, health, readiness or active issues change. A
summary watch polls the compact summary and prints again only when a revision (or the device health) moved, so it
stays cheap. Values that vary all the time (counters, signal strength) do not change the revision; `rohi status
<service> --watch` re-reads the detail on every poll and prints when what is shown changed. Stop with Ctrl-C.

## Doing things: actions

Status never changes anything. Operations are **actions**, listed and described by the device:

```console
$ rohi action list
SERVICE   ACTION      ACCESS  DESCRIPTION
network   scan        read    Scan for networks, strongest first
network   reconnect   write   Drop the connection and connect again
network   disconnect  write   Disconnect and stay idle
identity  provision   write   Install the device identity; fails if one exists
identity  erase       write   Remove the device identity

$ rohi action describe network scan
$ rohi action run network scan
SSID    RSSI     CHANNEL  SECURITY
office  -50 dBm  6        wpa2
guest   -70 dBm  11       open
```

`write` actions need a link that satisfies the device's authorization policy. Arguments are `NAME=VALUE` pairs; use
`NAME=-` to read a value from stdin so it stays out of the shell history. Secret arguments, like the one of identity
provisioning, are always read from stdin or a hidden prompt: `rohi action run identity provision secret=-`.

## Configuration

`rohi config` lists the sections of the device, describes their fields, and changes them with patches that are applied
atomically per section. Secrets are write-only. It is covered on [The rohi tool](rohi-tool.md#configuration); the
rules are in [Service pipeline](../sdk/service.md#config-sections).

## What the services are

| Service | What it reports | Typical issue codes |
|---------|-----------------|---------------------|
| `wifi` | connection state, network, signal, channel, radio heap | `authentication_failed`, `network_not_found`, `association_failed` |
| `network` | IPv4 address, gateway, DNS, address mode, socket usage | `dhcp_failed` |
| `ble` | peripheral state, advertised name, link security | `driver_error` |
| `connectivity` | telemetry queue and counters, endpoint | `connection_timeout`, `unauthorized`, `no_identity`, `no_utc_time`, `no_location` |
| `time` | wall-clock time, source, age, failures | `network_unavailable`, `timeout`, `implausible` |
| `sensors` | `<sensor>.<measurement>` values in canonical units | `bmx280_error`, `sds011_degraded`, ... |
| `identity` | provisioned or not, public key | `not_provisioned` |
| `location` | where the device is and where that comes from ([Location](../sdk/location.md)) | `not_set` |

A firmware may offer more or fewer; `rohi status` shows what the connected device really has. Adding a service needs
no change to the tool: [Add a service](../guides/add-service.md).
