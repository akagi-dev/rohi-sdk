# Telemetry profiles and delivery routes

Telemetry has two independent halves that you configure with ordinary Actions of the device, through the generic
`rohi action` command (see [Add a control operation or an action](add-control-operation.md)):

* the **`telemetry` service** owns *what* a signed packet contains: two fixed **profile slots**, `0` and `1`. A profile has
  one **public section** and at most one recipient-**private section** that is encrypted to a recipient's public key. The
  packet format is fixed (Robonomics); there are no names and no format choice;
* the **`connectivity` service** owns *where and how often* a profile is delivered: two fixed **route slots**, `0` and
  `1`. A route has a profile slot, an interval, a primary endpoint (an opaque URI: `http://host[:port]`, or `meshtastic+uart://!xxxxxxxx` in a build with the
  [Meshtastic backend](meshtastic.md)) and at most one **fallback** endpoint.

Indices are stable: changing or clearing one slot never moves the other. A profile exists without any route (and
`get_payload` works without a network); a route only references a profile slot. Both profile slots start empty and disclose
nothing, and the device sends nothing until you set an endpoint on a route slot.

```sh
# Profile 0, for the community: the BME280 and PM10, in clear.
rohi telemetry set_public 0 bmx280,sds011.pm10

# Profile 1, for the owner: PM values in clear, the position and noise only for one key (a 32-byte Ed25519 key).
rohi telemetry set_public 1 sds011
rohi telemetry set_private 1 \
  0xd75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a \
  location,microphone.average
rohi telemetry set_private 1 0xd75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a none   # remove it

rohi telemetry measurements 0          # the catalog: stable bit numbers and names the format can encode
rohi status telemetry                  # per profile: profile0_public, profile0_private, profile0_priv_selection, profile0_revision

# Deliver profile 0 every minute to an ingress from route slot 0, with one fallback for the same packet.
rohi connectivity set_endpoint 0 0 http://ingress.example.org
rohi connectivity set_interval 0 120
rohi connectivity set_fallback 0 http://backup-a.example.org
rohi connectivity set_fallback 0 ""                                  # no fallback
rohi connectivity set_endpoint 0 0 ""                                # no route: the slot is empty
rohi status connectivity               # per slot: route0_profile, route0_endpoint, route0_fallback, route0_state, route0_queue_depth, ...

# The sealed, signed packet of profile 1, with no route and no network (a standard stream).
rohi connectivity get_payload 1 > packet.bin
```

An **absent endpoint** is the empty string (`""`): `set_endpoint` with it deletes the route and `set_fallback` with it
removes the fallback. Positional arguments and named ones (`rohi action connectivity set_endpoint index=0 profile_index=0 endpoint=`) are equivalent.

Selections (`bmx280,sds011.pm10`, `all`, `none`) are names of the catalog; a sensor name stands for all of its
measurements. `all` never includes the position: `location` must be named. Names are turned into a 32-byte bitset by the
`rohi` client from the catalog Action; the device only ever sees the bits.

## Rules worth knowing

* **A slot is reusable, never stale.** Changing the profile or the endpoint of a route, or deleting it and setting it again,
  starts a new route: it inherits no queued packet, counter, backoff or generation. A new route starts with an interval of
  60 s. A fallback is another way to deliver the *same* packet, not another publisher.
* **Policy changes win.** Changing a profile (or clearing it by a factory reset) invalidates the packets that were built under the old
  policy before anything is sent again.
* **Delivery.** An acknowledgement from any path completes a packet: HTTP `202`, or the matching routing ACK of every Meshtastic
  fragment (which proves the mesh delivered them, not that the gateway accepted the packet). A path that cannot carry
  *this* packet (too large for the mesh) hands it to the other path without a penalty.
  For HTTP: Failures try the next path at once; `401`/`403` also pause the
  failing path for ten minutes; `400`/`413` drop the packet without offering it to other endpoints. Retries send
  byte-identical bytes. Delivery is at-least-once, so the ingress deduplicates by the signed envelope.
  Every request carries `User-Agent: <build>`, the `build` of `rohi info`; it is not part of the signed payload.
* **Honest dating.** A packet is dated at its capture; without a synchronized UTC clock, an identity or entropy the
  packet is refused, never faked. The state of the last attempt is in `rohi status connectivity`.
* **Stored data.** There is no migration: the persisted `telemetry` and `connectivity` sections of the earlier named
  profiles and routes are not read. A device that stores one reports it in the log and starts with empty slots; the next
  change replaces it.
* **Limits.** 2 profile slots with 1 private recipient each and 2 route slots with 1 fallback each: these are compile-time
  capacities of the Altruist Urban firmware (the storage is sized for them, a stored table over capacity is refused, not
  truncated, and the `connectivity` status schema has exactly two route slots, with the fields of each slot in the flat status as
  `route0_*` and `route1_*`). Endpoints are up to 64 bytes, checked with the schemes the build supports before they are stored, all within
  the 1 KiB persistent section of each service. What the Robonomics format cannot encode (the SCD4x, for example) is not in
  the catalog and is rejected, never published under another name.

Reference: [`rohi-telemetry`](https://docs.rs/rohi-telemetry),
[`rohi-telemetry-robonomics`](https://docs.rs/rohi-telemetry-robonomics) and
[`rohi-connectivity`](https://docs.rs/rohi-connectivity). The wiring of Altruist Urban is in
`firmwares/altruist-urban/src/{telemetry,connectivity}.rs`.
