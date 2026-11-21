# Status & diagnostics

This page explains how to see what each service of a device is doing and how that differs from configuring it, using `rohi`. Nothing here is specific to a module: the device describes its services, sections and actions, and the tool shows them. How a firmware reports status is in [Status](../../build/status.md).

## Commands

```bash
rohi status                       # device health and one row per service: one compact request
rohi status network                  # one service: values with units, active issues
rohi status --watch               # reprint when something observable changed (Ctrl-C to stop)
rohi status network --watch --interval 5
rohi -o json status network
```

Services are discovered from the device, so `rohi status <name>` works for any service the summary lists. Without a
service the output is the summary table; with `-o json` it is
`{ "device": .., "uptime_ms": .., "health": .., "services": [..] }`. The sections below explain health,
issues and revisions.

## Four questions, four places

```text
Capability    what can this firmware do?          (kept for runtime operations, e.g. firmware update)
Config        what is it set up to do?            persistent, desired state      rohi config ...
Status        what is it actually doing now?      live, observed state, read-only rohi status ...
Actions       what can I ask it to do?            explicit operations            rohi action ...
Diagnostics   what happened before?               logs, history                  rohi logs ...
```

The same service appears in several places, never mixed:

```console
$ rohi config connectivity          # desired: what it is set up to do (defaults + your overrides)
enabled     true
node_id     42
interval_s  60 s
host        ingress.example.org
port        443

$ rohi status connectivity          # observed: what is happening now
Service:  connectivity
Health:   degraded

frames_created           212
frames_delivered         209
send_retries             7
frames_rejected          0
queue_depth              3
frames_without_location  212

Issues:
  WARNING  no_location  (34s)
```

## The device summary

`rohi status` is **one compact request**: the device health and one row per service. It stays small however many
fields a service has.

```console
$ rohi status
Device:  altruist-urban-0.4.0 (urban-esp32c3)
Uptime:  2h 14m
Health:  degraded

SERVICE       HEALTH
network       ok
connectivity  degraded
time          ok
sensors       ok
identity      ok
location      -
```

**Health is the one common indicator** of a service:

| Health | Meaning |
|--------|---------|
| `ok` | working |
| `degraded` | works with reduced capability, or something is wrong (telemetry without a location, a Wi-Fi that lost its link) |
| `error` | not working |
| `-` (unknown) | not known yet (Wi-Fi is still connecting) or not in use: turned off by configuration or never set, which is **not an error** |

What else a service has to tell, such as a link state or a sensor state, is a value of that service in
`rohi status <service>`.

### Device health

`Health:` in the header is decided **by the firmware**, not computed by the tool. The firmware knows which services
are mandatory, optional or redundant: a cellular modem may be degraded while Ethernet is fine. With the default policy
an essential service with `error` health makes the device `error`, a degraded service makes it `degraded`, and
services with `unknown` health do not count.

## One service in detail

`rohi status <service>` adds the service-specific values the device describes (with their units) and the **active
issues**:

```console
$ rohi status wireless
Service:  wireless
Health:   ok

wifi_state       connected
wifi_ip          192.168.1.42
wifi_rssi        -61 dBm
wifi_reconnects  0
heap_free        41200 B
```

An **issue** has a stable machine-readable code (`connection_timeout`, `no_location`), a severity, a message and an
age. Scripts react to the code, people read the message. Only issues that are active *now* are listed; history is
diagnostics. With `-o json` the values are keyed by field name and carry their units:

```console
$ rohi -o json status wireless
{
  "schema_version": 1,
  "service": { "service": "wireless", "health": "ok", "revision": 3, ... },
  "values": { "wifi_state": "connected", "wifi_rssi": -61, "wifi_ip": "192.168.1.42" },
  "units": { "rssi": "dBm" },
  "issues": []
}
```

## Watching

```console
$ rohi status --watch                 # reprint when something observable changed
$ rohi status connectivity --watch --interval 5
```

Every service has a **status revision** that changes when its health or active issues change. A
summary watch polls the compact summary and prints again only when a revision (or the device health) moved, so it
stays cheap. Values that vary all the time (counters, signal strength) do not change the revision; `rohi status
<service> --watch` re-reads the detail on every poll and prints when what is shown changed. Stop with Ctrl-C.

## Doing things: actions

Status never changes anything. Operations are **actions**, listed and described by the device:

```console
$ rohi action
SERVICE   ACTION         ACCESS
wireless  wifi_scan      read
wireless  wifi_reconnect mutate
wireless  wifi_disconnect mutate
identity  provision      provision
identity  erase          mutate
system    reboot         mutate
system    factory_reset  provision

$ rohi action wireless                 # the actions of one service, with their arguments
$ rohi action wireless wifi_scan --help  # arguments and results, from the device schema
$ rohi action wireless wifi_scan
SSID    RSSI     CHANNEL  SECURITY
office  -50 dBm  6        wpa2
guest   -70 dBm  11       open
```

Every action needs an access level (`read`, `mutate` or `provision`) that the link must satisfy under the device's authorization policy. Arguments are `NAME=VALUE` pairs; use
`NAME=-` to read a value from stdin so it stays out of the shell history. Secret arguments, like the one of identity
provisioning, are always read from stdin or a hidden prompt: `rohi action identity provision secret=-`.

## Configuration

`rohi config` lists the sections of the device, shows the **effective** configuration of a section (the firmware
defaults with your explicit **overrides** applied) and changes overrides with patches that are applied atomically per
section. Secrets are write-only. It is covered on [Configuration](configuration.md); the rules are in
[Service pipeline](https://docs.rs/rohi-service).

## What the services are

| Service | What it reports | Typical issue codes |
|---------|-----------------|---------------------|
| `wireless` | Wi-Fi state, IPv4 address, signal and reconnects, radio heap; the BLE peripheral shows up in the health and the issues only | `authentication_failed`, `network_not_found`, `dhcp_failed`, `driver_error` |
| `connectivity` | telemetry queue and counters | `connection_timeout`, `unauthorized`, `no_identity`, `no_utc_time`, `no_location` |
| `time` | wall-clock time, source, age, failures | `network_unavailable`, `timeout`, `implausible` |
| `sensors` | `<sensor>.<measurement>` values in canonical units | `bmx280_error`, `sds011_degraded`, ... |
| `identity` | provisioned or not, public key | `not_provisioned` |
| `location` | where the device is and where that comes from ([Location](https://docs.rs/rohi-location)) | `not_set` |

A firmware may offer more or fewer; `rohi status` shows what the connected device really has. Adding a service needs
no change to the tool: [Add a service](../../guides/add-service.md).
