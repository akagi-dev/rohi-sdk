# Location (`rohi-location`)

`rohi-location` answers one question for the rest of the firmware: **where is the device?** Today the answer is what the
user stored in the `location` config section; a GNSS receiver can later publish a measured fix through the same seam.
There is no GNSS runtime in this repository yet. Telemetry puts the answer into every frame as the `GPS` entry
([Telemetry](connectivity.md)).

A location is optional. A device that was never told where it is has none; nothing invents one and nothing substitutes
`0, 0`. The section is edited like any other, for example `rohi config location latitude=52.52 longitude=13.405` and
`rohi status location` ([Service pipeline](service.md), [Status](../use/desktop/status.md)). Latitude and longitude exist
only as a pair, `rohi config reset location` makes the device not know where it is again, and the change applies at once
without a restart ([Async and cancellation](../concepts/async-and-cancellation.md#durable-mutation)).

Two things to know as a user: the location is public, **unencrypted** telemetry, so anyone who can read the frames sees
where the device is; and without a location telemetry is still sent, but the `connectivity` service reports `degraded`
with the warning `no_location` until one is set.

The crate is a small, complete example of a feature with a config section, a shared runtime value and a status service
([Add a service](../guides/add-service.md)). The section's persistent bytes are pinned, see
[Persistent ABI](../reference/persistent-abi.md).

| Item | Role |
|------|------|
| `LocationConfig`, `LocationOverrides` | The `location` section and its persisted overrides |
| `LocationShared` | Current location: a GNSS fix wins while published, otherwise the configured position |
| `Location`, `LocationSource`, `NoLocation` | What consumers (telemetry) read, independent of the provider |
| `LocationService` | The `location` status service (`disabled` with `not_set` until a location is set) |

Status: implemented and host-tested; GNSS input is not implemented.

## Reference

The crate rustdoc is the authoritative technical documentation (ranges, validation, source priority, telemetry
behaviour):

- `rohi-location`: [docs.rs](https://docs.rs/rohi-location), [source](https://github.com/akagi-dev/rohi-sdk/tree/master/crates/location)
- SDK-level concepts: [docs.rs/rohi-sdk](https://docs.rs/rohi-sdk)
- Locally: `cargo doc --no-deps --open -p rohi-location`
