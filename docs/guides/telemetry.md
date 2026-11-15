# Telemetry profiles and delivery routes

Telemetry has two independent halves that you configure with ordinary Actions of the device, through the generic
`rohi action` command (see [Add a control operation or an action](add-control-operation.md)):

* the **`telemetry` service** owns *what* a signed packet contains: persistent **packet profiles** that name a format,
  a **public section** and zero or more recipient-**private sections** that are encrypted to a recipient's public key;
* the **`connectivity` service** owns *where and how often* a profile is delivered: **routes** with an interval, a
  primary HTTP endpoint and ordered **fallback** endpoints.

A profile exists without any route (and `packet_get` works without a network); a route only references a profile. A new
profile discloses nothing, and the device sends nothing until you add a route.

```sh
# A profile for the community: the BME280 and PM10, in clear.
rohi telemetry profile_add community robonomics
rohi telemetry set_public community bmx280,sds011.pm10

# A profile for the owner: PM values in clear, the position and noise only for one key (a 32-byte Ed25519 key).
rohi telemetry profile_add owner robonomics
rohi telemetry set_public owner sds011
rohi telemetry add_private owner \
  0xd75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a \
  location,microphone.average

rohi telemetry measurements 0          # the catalog: stable bit numbers and names the format can encode
rohi telemetry profile_list 0
rohi telemetry profile_recipients owner 0

# Deliver a profile every minute to an ingress, with two fallbacks for the same packet.
rohi connectivity route_add community community http://ingress.example.org
rohi connectivity set_interval community 120
rohi connectivity fallback_add community backup-a http://backup-a.example.org
rohi connectivity fallback_add community backup-b http://backup-b.example.org
rohi connectivity route_list 0
rohi connectivity fallback_list community 0

# The sealed, signed packet of a profile, with no route and no network (a standard stream).
rohi connectivity packet_get owner > packet.bin
```

Selections (`bmx280,sds011.pm10`, `all`, `none`) are names of the catalog; a sensor name stands for all of its
measurements. `all` never includes the position: `location` must be named. Names are turned into a 32-byte bitset by the
`rohi` client from the catalog Action; the device only ever sees the bits.

## Rules worth knowing

* **Immutable routes.** The id, profile and primary endpoint of a route cannot be changed: remove the route and add it
  again (a new route inherits no queue and no backoff). Only the interval changes in place. A fallback is another way to
  deliver the *same* packet, not another publisher.
* **Policy changes win.** Editing or deleting a profile invalidates the packets that were built under the old policy
  before anything is sent again; a deleted and recreated profile is never silently picked up by an old route.
* **Delivery.** Only HTTP `202` acknowledges a packet. Failures try the next path at once; `401`/`403` also pause the
  failing path for ten minutes; `400`/`413` drop the packet without offering it to other endpoints. Retries send
  byte-identical bytes. Delivery is at-least-once, so the ingress deduplicates by the signed envelope.
* **Honest dating.** A packet is dated at its capture; without a synchronized UTC clock, an identity or entropy the
  packet is refused, never faked. The state of the last attempt is in `rohi status connectivity`.
* **Limits.** 3 profiles with 2 recipients each, 3 routes with 2 fallbacks each, endpoints up to 64 bytes, all within the
  1 KiB persistent section of each service. What the Robonomics format cannot encode (the SCD4x, for example) is not in
  the catalog and is rejected, never published under another name.

Reference: [`rohi-telemetry`](https://docs.rs/rohi-telemetry),
[`rohi-telemetry-robonomics`](https://docs.rs/rohi-telemetry-robonomics) and
[`rohi-connectivity`](https://docs.rs/rohi-connectivity). The wiring of Altruist Urban is in
`firmwares/altruist-urban/src/{telemetry,connectivity}.rs`.
