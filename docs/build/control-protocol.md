# Control protocol

The **control protocol** is the shared contract that lets a generic application operate any firmware. It exists so
that a firmware author describes a device once, in [Config, Status and Action](core-concepts.md), and every client can
use it without a firmware-specific integration.

```text
 Firmware        sections · services · actions
    │            described by schemas the device publishes
 Protocol        request/response messages (protobuf), one schema, bounded sizes
    │
 Transport       moves bytes: BLE today (GATT), others possible
    │
 Application     rohi desktop · mobile app (planned) · your client
```

## What it does

- **Makes capabilities discoverable.** Clients ask what sections, services and actions exist and fetch their schemas. Config
  becomes readable and editable settings, Status becomes health and metrics, Action becomes callable operations.
- **Avoids per-firmware clients.** The protocol, the dispatcher and the client know no module by name, so a new
  firmware module needs no client change.
- **Separates concerns.** The firmware owns meaning, the protocol owns the wire format and the transport owns delivery.
  The transport carries opaque bytes, so replacing it does not change the firmware or the clients.

## Current state

| Aspect | State |
|--------|-------|
| Protocol | version 1; **still under development** and changing without a version bump: keep client and firmware on the same revision |
| Transport | BLE on devices and, on Linux, on the [`std` Altruist Urban](../guides/linux-ble.md) (BlueZ); independently, TCP records (`tcp://HOST:PORT`: a 4-byte length and one Protobuf message, no BLE frames) for the [`std` Altruist Urban](../guides/altruist-urban-std.md#control-over-tcp-and-the-rohi-tool) |
| Security | each transport has an access mode: TCP is `restricted` by default (reads are open, changes need an Ed25519 signature of the one administrator key), BLE is `open` by default (pairing when set to `restricted`); no TLS, no confidentiality, see [Access control](#access-control) |
| Scope | local control only; telemetry uplink is a separate service |

## Access control

Requests have a **privilege**: `Read` (observe and discover), `Mutate` (change configuration, reboot, factory reset,
reconnect) or `Provision` (initialize protected state: the identity, the administrator key). Every action declares its
privilege in its descriptor, and the schema shows it. `secret_args` marks sensitive arguments; it does not make an action
privileged.

Each transport endpoint (`control_tcp`, `control_ble`) has an **access mode**, stored in its own config section and
reported in its own status:

| Request | `open` | `restricted`, TCP | `restricted`, BLE |
|---------|--------|-------------------|-------------------|
| Read | allowed | allowed | allowed |
| Mutate, Provision | allowed | a valid signature of the administrator key | an encrypted (paired) link |

The defaults are TCP `restricted` and BLE `open`. The modes are independent: a signature or a pairing on one transport
never authorizes anything on the other. A refusal is `PermissionDenied` with a stable reason (`auth_required`,
`no_admin_key`, `no_challenge`, `bad_signature`, `stale_sequence`, `nested`, `not_supported`, `pairing_required`) and the
service never runs.

**Signed requests (TCP).** The device trusts one 32-byte Ed25519 public key. A client asks `GetAuthChallenge` for a fresh
32-byte nonce bound to its connection, then wraps each privileged request in a `SignedRequest`:

```text
signature = Ed25519(admin secret,
    "rohi-control-auth-v1" || session_nonce[32] || sequence_le_u64[8] || request_bytes)
```

`request_bytes` are the exact bytes of one encoded `Request` (never a signed one or a challenge) with the same
`request_id` as the wrapper. The sequence must be strictly greater than the last accepted one of the connection; the
device advances it before it runs the request, so a lost answer never lets the same signature run twice. The nonce and
the sequence live in RAM, die with the connection and with every key change, and a connection that sends repeated bad
signatures is refused without more checks. The wrapper is `SignedEnvelope` in the schema (wire-identical to a `Request`
whose oneof is `signed_request`); the answer is the ordinary answer to the inner request.

**Limits.** A signature authenticates one request to the device. It does not encrypt the request or the answer, it does
not authenticate the device to the client, and a network attacker can still read, drop or relay traffic. There is no TLS or
Noise yet. BLE `open` is deliberately insecure for privileged operations: restricting TCP does not secure a device whose BLE
endpoint is open. Pairing on BLE is Just Works (LE Secure Connections, no bonding): it keeps a passive listener out, it is
not an authenticated link.

**Administrator key.** `control_auth.set_admin_key` (Provision) enrolls or replaces the key, `clear_admin_key` removes it,
and a factory reset removes it and restores the defaults. Without a key a restricted TCP endpoint refuses every change; it
never falls back to open. The first key has to arrive over an open (or paired) endpoint, usually BLE; a device whose key is
lost is recovered by a factory reset over such an endpoint. See [Access and the administrator key](../use/desktop/access.md).

## One control worker

BLE and TCP are thin frontends: they frame bytes (BLE fragments, TCP records), own their connections and hand each
request to a slot of a bounded mailbox. One `control_task` owns the one `Controller` and runs Config, Status, Actions and
streams for every transport, one request at a time. A connection has one request in flight; a full mailbox is answered
`Busy`. A long action therefore delays the requests of other connections. Every connection has a session identity that is
unique across transports; closing it releases only its streams and its challenge.

## Where to go next

- Operate a device: [Desktop application](../use/desktop/index.md), [Mobile application](../use/mobile.md) (planned).
- The authoritative definition is the schema [`control.proto`](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/control-proto/proto/rohi/control/v1/control.proto)
  and the rustdoc of `rohi-control-proto`, `rohi-control` and `rohi-control-ble`, see [SDK crates](sdk-crates.md).
- Extend the protocol: [Add a control operation or an action](../guides/add-control-operation.md).
