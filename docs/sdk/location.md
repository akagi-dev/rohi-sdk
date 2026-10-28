# Location (`rohi-location`)

`rohi-location` answers one question for the rest of the firmware: **where is the device?** Today the answer is what
the user stored; later a GNSS receiver will measure it. Telemetry puts the answer into every frame.

```text
  user ── config section `location` ──┐
                                      ├─► LocationShared ──► LocationSource ──► telemetry frames (GPS entry)
  GNSS receiver (future) ─────────────┘          │
                                                 └─► `location` service (rohi status location)
```

A location is **optional**. A device that was never told where it is has none; nothing invents one, and nothing
substitutes `0, 0`.

## Set the location

The location is an ordinary [config section](service.md#config-sections) called `location`:

```console
$ rohi config location latitude=52.52 longitude=13.405 height_m=34
latitude   52.52 deg
longitude  13.405 deg
height_m   34 m

$ rohi config location                    # the effective configuration; nothing set shows <unset>
latitude   52.52 deg
longitude  13.405 deg
height_m   34 m

$ rohi status location
Service:  location
State:    running
Health:   ok
Ready:    yes
Since:    3s

latitude   52.52 deg
longitude  13.405 deg
height_m   34 m
source     configured
```

| Field | Type | Range | Required |
|-------|------|-------|----------|
| `latitude` | `f32` on the control plane, degrees (WGS84) | -90 … 90 | yes |
| `longitude` | `f32` on the control plane, degrees (WGS84) | -180 … 180 | yes |
| `height_m` | `f32` on the control plane, metres above mean sea level | -1000 … 10000 | no (`rohi config location height_m=null` removes the override) |

Single precision resolves a coordinate to about one to two metres; the overrides are stored as `f64` (see [persistent
ABI](../reference/persistent-abi.md)) and the control plane converts.

Latitude and longitude only exist as a pair: the default (no override) is not a position (`NaN`, shown as `<unset>`) and is a valid, unconfigured section, so a first
`height_m=10` is refused instead of storing `0, 0`. NaN and infinite values are refused too. The device validates
the complete effective result before anything is stored (see [Config sections](service.md#config-sections));
a section without coordinates is valid. `rohi config reset location` removes every override: the device does not know where it is again.

The change takes effect at once, without a restart: the firmware's configuration owner task hands every commit or
removal to the shared location in the commit step, like the other runtime sections
([Durable mutation](../concepts/async-and-cancellation.md#durable-mutation)).

## Sources and priority

`LocationShared` holds two optional values and `current()` returns the better one:

| Source | Published by | When it wins |
|--------|--------------|--------------|
| `gps` | a GNSS runtime with `publish_fix(Some(..))` | while it publishes a fix |
| `configured` | the configuration owner with `publish_configured` | when there is no fix |

There is no expiry in `rohi-location`: the GNSS runtime knows what a stale fix is and must publish `None` as soon as
the fix is lost, which makes the device fall back to the configured position. The GNSS runtime does not exist yet;
`publish_fix` and the rule are covered by unit tests and are the only seam a future receiver needs. Consumers see a
`Location { latitude, longitude, height_m, source }` through the `LocationSource` trait and never depend on the
provider.

## Telemetry

`rohi-connectivity` reads the location through `LocationSource` when it creates a frame and adds the
`sensor.v1.GPS` entry as the **first** entry of `Urban.public`: WGS84 degrees, height `0` when the height is not known
(as the schema defines). Consequences:

* The location is **context, not a measurement**. It is not counted as emitted, and a snapshot without a valid
  measurement is still not sent, even if a location is known.
* The location is public, **unencrypted** telemetry. Anyone who can read the frames sees where the device is. Remove the
  overrides (`rohi config reset location`) to stop sending it.
* Without a location the frame is still sent, without the entry, and counted in
  `Diagnostics::frames_without_location` (`frames_without_location` in `rohi status connectivity`).
* A position that is not a finite number is left out and counted as out of range.

### Connectivity is degraded without a location

Telemetry without a position is incomplete, so while telemetry is running and the device has no location, the
`connectivity` service reports

```console
$ rohi status connectivity
Service:  connectivity
State:    running
Health:   degraded
Ready:    yes
...
Issues:
  WARNING  no_location  (12s)
           No location is set; telemetry is sent without it
```

`ready` stays `yes` because frames are delivered. The device health follows the default policy and becomes
`degraded`; `rohi status` shows it. Disabled telemetry is not degraded by a missing location. The fix is one command:
`rohi config location latitude=.. longitude=..`.

## Service

`LocationService` is the `location` service of the [status plane](../getting-started/services.md): `disabled` (with the
informational issue `not_set`) until a location is set, then `running`, `ok`, ready. Status fields: `latitude`,
`longitude`, `height_m` (absent when unknown) and `source` (`configured` or `gps`). It is not essential for the device.

## Persistent ABI

`LocationConfig` is the effective section and `LocationOverrides` (generated by `#[derive(ConfigOverrides)]`) its persisted overrides, both owned by this crate: section type name `LocationConfig`, section ID
`0xce098b4a3e381f40`, version 1, CBOR map `{1: latitude, 2: longitude, 3: height_m}` of 64-bit floats (a key is absent
when the field is not overridden; the default has `NaN` coordinates, which read as "no value"). A golden test pins the bytes and the ID; see [Persistent ABI](../reference/persistent-abi.md).

## Firmware wiring

`altruist-urban` owns one static `LocationShared` (`location.rs`). The configuration owner publishes the stored
section at boot and after every commit (`config.rs`), the connectivity task reads it for every frame and the
control registries list the section and the service (`control.rs`). Adding a GNSS source means publishing its fix to
the same `LocationShared`.
