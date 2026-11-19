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
| Security | each transport has an access mode: TCP is `restricted` by default (a handshake with a 32-byte pre-shared key, then authenticated and encrypted records; no forward secrecy), BLE is `open` by default (pairing when set to `restricted`); no TLS, see [Access control](#access-control) |
| Scope | local control only; telemetry uplink is a separate service |

## Access control

Requests have a **privilege**: `Read` (observe and discover), `Mutate` (change configuration, reboot, factory reset,
reconnect) or `Provision` (initialize protected state: the identity, the TCP pre-shared key). Every action declares its
privilege in its descriptor, and the schema shows it. `secret_args` marks sensitive arguments; it does not make an action
privileged.

Each transport endpoint (`control_tcp`, `control_ble`) has an **access mode**, stored in its own config section and
reported in its own status:

| Request | `open` | `restricted`, TCP | `restricted`, BLE |
|---------|--------|-------------------|-------------------|
| Read | allowed | allowed | allowed |
| Mutate, Provision | allowed | an authenticated and encrypted connection (the handshake with the pre-shared key) | an encrypted (paired) link |

The defaults are TCP `restricted` and BLE `open`. The modes are independent: a handshake or a pairing on one transport
never authorizes anything on the other. A refusal is `PermissionDenied` with a stable reason (`no_psk`,
`encryption_required`, `pairing_required`) and the service never runs.

**Security belongs to the transport.** The common control layer (`rohi-control`) implements no encryption, no signature and no
challenge: it only reads what the transport established, as the encrypted and authenticated flags of the link, and nothing in a
request can change them. An action that carries key material of a transport (`control_psk.set_tcp_psk`) is additionally marked
`secure_link`: in every access mode it runs only on a link the transport has protected (an encrypted BLE link, or the
authenticated TCP connection of the current key) and is refused as `encryption_required` or `pairing_required` otherwise.

**Secure channel (TCP).** A restricted TCP endpoint serves only clients that complete a handshake with a 32-byte
pre-shared key (PSK), and every record after it is authenticated and encrypted. Nothing is read or processed before the
handshake verified, and there is never a fallback to plaintext:

```text
ClientHello  "ROHI" | version 1 | client_nonce[32]
ServerHello  "ROHI" | version 1 | server_nonce[32] | server_proof[32]
ClientFinish client_proof[32]
prk = HKDF-Extract(salt = client_nonce || server_nonce, ikm = psk)           (SHA-256)
server_proof, client_proof = HKDF-Expand(prk, label || SHA-256(transcript) [|| server_proof])
record = version(1) | length u16 BE | ciphertext | tag(16)    XChaCha20-Poly1305, header as AAD,
         nonce = prefix(12) || counter u64 BE || direction u32   (directional keys, counters count records)
```

The two proofs are bound to the whole transcript, so both sides are authenticated and the nonces of one connection are
useless in another. A malformed, replayed, reordered, truncated or tampered record, a counter that is used up and a key that
was replaced all end the connection with nothing written in plaintext. The key is never sent or logged and the derived keys
are wiped on drop. This is implemented by `rohi-control-tcp` alone (`rohi_control_tcp::secure`); BLE keeps the link security of
its host stack and gets no framing encryption.

**Limits.** The PSK design has **no forward secrecy**: whoever learns the key later can decrypt a recorded session and
impersonate the device or a client. There is no TLS, no certificate and no key agreement beyond the PSK. A network attacker can
still drop or delay traffic. BLE `open` is deliberately insecure for privileged operations: restricting TCP does not secure a device whose BLE
endpoint is open. Pairing on BLE is Just Works (LE Secure Connections, no bonding): it keeps a passive listener out, it is
not an authenticated link.

**TCP key.** `control_psk.set_tcp_psk` (Provision, `secure_link`) sets or replaces the key, `clear_tcp_psk` removes it, and a
factory reset removes it and restores the defaults. Without a key a restricted TCP endpoint serves nobody; it never falls back
to open or to plaintext. There is no unauthenticated TCP bootstrapping: the first key arrives over an encrypted (paired) BLE
link, a device whose key is lost is recovered by a factory reset over such a link, and rotation is done over a link the old key
or BLE protects (the TCP connections of the old key end). The `std` host can also take the key from a file for one run
(`--control-psk-file`). See [Access and the TCP key](../use/desktop/access.md).

## One control worker

BLE and TCP are thin frontends: they frame bytes (BLE fragments, TCP records), own their connections and hand each
request to a slot of a bounded mailbox. One `control_task` owns the one `Controller` and runs Config, Status, Actions and
streams for every transport, one request at a time. A connection has one request in flight; a full mailbox is answered
`Busy`. A long action therefore delays the requests of other connections. Every connection has a session identity that is
unique across transports; closing it releases only its streams.

## Where to go next

- Operate a device: [Desktop application](../use/desktop/index.md), [Mobile application](../use/mobile.md) (planned).
- The authoritative definition is the schema [`control.proto`](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/control-proto/proto/rohi/control/v1/control.proto)
  and the rustdoc of `rohi-control-proto`, `rohi-control` and `rohi-control-ble`, see [SDK crates](sdk-crates.md).
- Extend the protocol: [Add a control operation or an action](../guides/add-control-operation.md).
