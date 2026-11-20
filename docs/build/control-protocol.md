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
| Transport | BLE on devices and, on Linux, on the [`std` Altruist Urban](../guides/linux-ble.md) (BlueZ); independently, TCP (`tcp://HOST:PORT`: a preamble, then either 4-byte-length Protobuf records or the PSK handshake and encrypted records; no BLE frames) for the [`std` Altruist Urban](../guides/altruist-urban-std.md#control-over-tcp-and-the-rohi-tool) |
| Security | TCP is `protected` by default (plaintext reads; a handshake with a 32-byte pre-shared key unlocks everything, with authenticated and encrypted records; no forward secrecy); **BLE is open** (no pairing, no link encryption) in this build, an accepted insecure state; no TLS, see [Access control](#access-control) |
| Scope | local control only; telemetry uplink is a separate service |

## Access control

Requests have a **privilege**: `Read` (observe and discover), `Mutate` (change configuration, reboot, factory reset,
reconnect) or `Provision` (initialize protected state: the identity, the TCP pre-shared key). Every action declares its
privilege in its descriptor, and the schema shows it. `secret_args` marks sensitive arguments; it does not make an action
privileged.

The TCP endpoint has an **access mode**, the `tcp_access_mode` field of the `control` config section (it applies after a
restart). The BLE endpoint has none: it is open. There is one public service, `control`, for both.

| Request | TCP `open` | TCP `protected` | TCP `private` | BLE |
|---------|------------|-----------------|---------------|-----|
| Read, no key | allowed | allowed | denied | allowed |
| Mutate, no key | allowed | denied | denied | allowed |
| Provision, no key | allowed | denied | denied | allowed |
| Any request after the handshake with the key | allowed | allowed | allowed | n/a (BLE link: open, **not** encrypted and **not** authenticated) |

The default is TCP `protected`. The mode is authorization, not link confidentiality: `secure_link` actions (below) are an
additional rule in every mode. A missing key does not change the state of the endpoint (`tcp_state` stays `running`); it only
makes the authenticated rows impossible until a key is generated. An unencrypted BLE link is never reported as `encrypted` or `authenticated`; the policy
decides from the properties the transport reports, never from a configuration flag, so a restricted BLE mode can return
without changing the transports. A refusal is `PermissionDenied` with a stable reason (`no_psk`, `encryption_required`) and
the service never runs.

**Security belongs to the transport.** The common control layer (`rohi-control`) implements no encryption, no signature and no
challenge: it only reads what the transport established, as the encrypted and authenticated flags of the link, and nothing in a
request can change them. An action that carries key material of a transport (`control.tcp_generate_psk`) is additionally marked
`secure_link`: on TCP it runs only on the authenticated and encrypted connection of the current key and is refused as
`encryption_required` otherwise, so `open` never lets the key travel in plaintext. Open BLE counts as allowed for it in this phase: that is the one place where a secret
reaches an open link, and it is an accepted exception, not a design.

**Preamble (TCP).** A TCP connection starts with an explicit preamble that says how it goes on; nothing is guessed from
other bytes:

| First bytes | Meaning | Served in |
|-------------|---------|-----------|
| `"ROHP"` \| version 1 (5 bytes) | plaintext records follow: a 4-byte little-endian length and one Protobuf message each | `open`, `protected` |
| `"ROHI"` \| version 1 \| client_nonce (the hello below) | the PSK handshake, then encrypted records | every mode |
| anything else, or another version | the connection is closed, one transport error is counted | n/a |

A plaintext connection is never authenticated and never encrypted: the policy of the mode decides what it may do. `private`
closes a plaintext connection after the first four bytes (an access refusal, not an error). A client that fails the
handshake is not served in plaintext afterwards, and a client cannot declare itself authenticated in a request.

**Secure channel (TCP).** A client that completes the handshake with a 32-byte pre-shared key (PSK) may do everything in
every mode, and every record after it is authenticated and encrypted. Nothing is read or processed before the handshake
verified, and there is never a fallback to plaintext:

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
still drop or delay traffic. BLE is open in this build: anyone in radio range can change the configuration, provision the identity, reset the device and
run `tcp_generate_psk`; restricting TCP does not secure a device that has BLE control. Telemetry payload encryption is a
separate, packet-level property and is not affected.

**TCP key.** The device always generates the key. `control.tcp_generate_psk` (Provision, `secure_link`) makes a new random key on
every call, stores it, ends the TCP connections of the old one and returns the new key in its result; no client chooses a key
and no action reads the current one. A factory reset removes it and restores the defaults. Without a key a `private` TCP
endpoint serves nobody and a `protected` one only reads; a client that asks for the handshake is refused and never falls back to plaintext. The first key is therefore fetched over BLE (open, so in
the clear) or over the connection of an existing key; a device whose key is lost is recovered with `tcp_generate_psk` or a factory
reset. The `std` host can also take the key from a file for one run (`--control-psk-file`). See [Access and the TCP key](../use/desktop/access.md).

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
