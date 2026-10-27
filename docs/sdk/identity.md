# Device identity

Every provisioned ROHI device has exactly one Ed25519 identity. It is **provisioned from the host, never generated
by the device**, and becomes the cryptographic identity used to sign telemetry.

```text
master SURI ──► rohi ──► hard derivation //<number> ──► derived Ed25519 secret
                                                              │ control protocol (ProvisionIdentity)
                                                              ▼
                              ConfigStore ◄── Identity section ◄── device
                                                              │
                                                       IdentitySigner ──► sign(exact bytes)
```

The master seed and the provisioning number exist only on the provisioning host. The device stores only its own
derived secret; the public key is derived from it and never persisted.

## Crate `rohi-identity`

`no_std`, no transport dependency (no BLE, Wi-Fi or Meshtastic). Built on `ed25519-zebra`.

| Item | Role |
|------|------|
| `Secret<T>` | Zeroized on drop; `Debug` prints `Secret(<redacted>)`; no `Display`. |
| `Identity` | The `Identity` `ConfigSection`; holds the 32-byte secret only. `public_key()`, `signer()`. |
| `PublicKey`, `Signature` | 32 / 64 raw bytes. `PublicKey` displays as `0x` + 64 hex digits and verifies signatures. |
| `Signer` | `public_key()` and `sign(&[u8])`; there is no way to get the private key out of it. |
| `IdentitySigner` | `Signer` backed by an `Identity`; `IdentitySigner::provisioned(None)` is `IdentityError::NotProvisioned`. |
| `MockSigner` | Test signer (feature `mock`). |

The signer signs **exactly the given bytes**. Protocol semantics (domain separation, `SignedEnvelope`, timestamp and
nonce for replay protection) belong to the Connectivity Protocol layer; the intended pipeline is
`MeasurementsSnapshot → protocol message → serialize once → IdentitySigner::sign(bytes) → SignedEnvelope`, with the
payload bytes never decoded or re-encoded after signing.

## Storage

The identity lives in the normal [configuration store](config.md), so it inherits A/B transactional writes,
generation tracking, CRC validation and power-loss recovery. There is no separate identity storage or partition.

* Section type `Identity`, ID `0x94d63573d312fe9b` (FNV-1a of the struct name), version 1.
* CBOR: `[bytes(32)]` (`#[cbor(array)]`), pinned by a golden byte test. This is persistent ABI.
* The identity is **not** a config section of the generic configuration API: `identity` is a service whose secret is a
  write-only action argument, so no config request can read, patch or reset it.
* Ordinary configuration changes, removing another section and OTA never touch it. Only the `erase` action removes it.
* A corrupted store is reported as unavailable, never as "not provisioned".

## Boot behaviour

On boot the firmware reads the `Identity` section. If it is absent the device is *not provisioned*: sensors, BLE
control and diagnostics keep working, and only operations that need the identity fail with `IdentityNotProvisioned`.
There is no automatic generation fallback. The log shows only the public key, never the secret.

The firmware keeps the result as runtime state (not provisioned, provisioned with its public key, or unknown when the
storage could not be read) next to the signer. `provision` and `erase` update it in the same step as the commit, and the
service reads only that state for its summary, issues and status: observing the identity does not read the persistent
store. Only an unknown state reads the store again, so a storage that was unavailable at boot recovers.

## Derivation (provisioning ABI)

The derived identity is the Substrate hard derivation of the master with the junction `//<number>`:

```text
number = 1    →  //1
number = 42   →  //42
number = 1000 →  //1000
```

* The path is exactly `//<number>`: no namespace (`//rohi//42`, `//sensor//42` are wrong). It must stay stable.
* The master is a Substrate secret URI (mnemonic phrase or `0x` seed, optional path and `///password`) read from a
  file. `rohi` applies the master's own path/password first and then the hard junction `//<number>`.
* Derivation uses `sp-core` (`ed25519`), the same algorithm as `Pair::from_string("<suri>//<number>")`.
* The same master and number give the same identity on every machine and in every future version, so an identity can
  be restored after flash replacement without backing up each derived key.
* **The number must be unique within one master hierarchy.** Using it for two active devices creates duplicate
  identities; the firmware cannot detect that because the number is never sent to it.

Golden vectors for the public development phrase (`bottom drive obey lake curtain smoke basket hold race lonely fit
walk`, never a production master) live in `tools/rohi/src/provision/tests.rs`:

| Number | Public key |
|-------:|------------|
| 1 | `0xbf3a763d817cee09bf785b9cc6118f58dab5c03f3ace6d524899bcb28ac74f27` |
| 2 | `0x276b9ce3c58b9f009f4f1f2ae5c7e9a599796246165df52f2d8b857fcecb1a40` |
| 42 | `0x8743ca72162320542b15ef0e66995fac97cd295626627046c41f64f796bcf2f3` |

## Control plane

The identity is the `identity` [service](service.md) of `rohi-identity` (`IdentityService` over an `IdentityStore` that
the firmware implements on its configuration handle):

| Request | Link privilege | Effect |
|---------|----------------|--------|
| `GetServiceStatus identity` | read | `provisioned` and the 32-byte `public_key` (absent when not provisioned) |
| `InvokeAction identity.provision { secret }` | provision (secret argument) | stores the identity, returns its public key |
| `InvokeAction identity.erase` | mutate | removes the identity section; no-op when absent |

`rohi status identity` shows the state; `rohi action run identity provision secret=-` runs the action.

* A status of `stopped` with the informational issue `not_provisioned` is the normal state of a fresh device.
* `provision` succeeds only when not provisioned, otherwise it is *rejected* with the reason `already_provisioned`. An
  existing identity is never silently replaced; erase it explicitly first.
* A secret that is not exactly 32 bytes is `INVALID_ARGUMENT`. After storing, the device reads the identity back and
  compares the public key before answering.
* No response or status ever carries the secret. The invoke request type has no `Debug` output and its arguments are
  overwritten after the call, so the secret cannot reach a log.
* Errors: `REJECTED` (`already_provisioned`), `INVALID_ARGUMENT`, `UNAVAILABLE`, `INTERNAL`, `PERMISSION_DENIED`.
* The master seed, the number and the derivation path are never sent to the device.

### Security status

Provisioning moves a private key over BLE, so it must only be accepted on a protected link. **BLE pairing is not
implemented yet** (see [Control plane](control.md#security)): the Altruist Urban firmware runs `OpenPolicy`, so today
anyone in radio range can provision or erase an identity. Provision and erase use a dedicated `Privilege::Provision`,
so `RequireEncryption` / `RequireAuthenticated` already reject them on an unprotected link with `PERMISSION_DENIED`
(covered by host tests); enabling pairing and switching the firmware policy is the remaining step. Provision devices
in a trusted place until then.

## `rohi` command line

There is no identity command: the generic `status` and `action` commands are used.

```bash
rohi status identity                          # state and public key; never the secret
rohi action run identity provision secret=-   # 32-byte secret as hex, read from stdin or a hidden prompt
rohi action run identity erase
```

The secret is derived on the host with `rohi::derive_identity(suri, number)` (the library of the `rohi` crate) and only
the derived secret is sent. The master seed, the number and the derivation path are never sent, stored or logged, and
a malformed secret is not echoed in the error. Compare the public key reported by `status identity` with
`DerivedIdentity::public_key()` after provisioning. `rohi factory-reset` formats the configuration storage and so erases the identity too.

## Firmware

`altruist_urban::identity` owns access to the identity: it loads it at boot, serves the control requests through
`ConfigHandle` and keeps an `IdentitySigner` for future signed telemetry (`IdentityHandle::sign`, which returns
`NotProvisioned` when erased).

The signer is updated by the **configuration owner task** in the same step as the commit or removal of the `Identity`
section, with no `.await` in between. Provision and erase therefore follow the durable-mutation rule: cancelled before
the request was queued, nothing changed; cancelled afterwards, the operation is logically completed and the signer
agrees with the stored identity ([Async and cancellation](../concepts/async-and-cancellation.md#durable-mutation)).

The secret passes through fixed-size buffers of the configuration channel and the BLE request path, which are not
scrubbed after use.

## Validation checklist (hardware)

1. Fresh Altruist Urban: `rohi status identity` reports *not provisioned*; sensors and BLE diagnostics work.
2. `rohi action run identity provision secret=-` with the secret derived from `<test-master>`: host and device public keys match.
3. Reboot: `rohi status identity` returns the same public key. Change unrelated configuration, reboot: unchanged.
4. `rohi action run identity erase`: state becomes *not provisioned*, signing returns `IdentityNotProvisioned`, sensors keep running.
5. Provision again with the same master and number: the original public key is restored.

## Out of scope

Automatic device-side generation, per-sensor keypairs, certificates, multiple identities, key rotation, secure
element, storing the number on the device, remote master storage, and Connectivity Protocol telemetry/uplink.
