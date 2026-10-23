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

The identity lives in the normal [configuration store](config-store.md), so it inherits A/B transactional writes,
generation tracking, CRC validation and power-loss recovery. There is no separate identity storage or partition.

* Section type `Identity`, ID `0x94d63573d312fe9b` (FNV-1a of the struct name), version 1.
* CBOR: `[bytes(32)]` (`#[cbor(array)]`), pinned by a golden byte test. This is persistent ABI.
* The generic configuration API (`GetConfig` / `UpdateConfig`) only knows Wi-Fi; the identity section is unreachable
  through it.
* Ordinary configuration changes, removing another section and OTA never touch it. Only `EraseIdentity` removes it.
* A corrupted store is reported as unavailable, never as "not provisioned".

## Boot behaviour

On boot the firmware reads the `Identity` section. If it is absent the device is *not provisioned*: sensors, BLE
control and diagnostics keep working, and only operations that need the identity fail with `IdentityNotProvisioned`.
There is no automatic generation fallback. The log shows only the public key, never the secret.

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

## Control protocol

| Request | Capability | Link privilege | Effect |
|---------|------------|----------------|--------|
| `GetIdentity` | `IDENTITY` | read | `IdentityInfo { state, public_key? }` |
| `ProvisionIdentity { ed25519_secret }` | `IDENTITY` | provision | stores the identity, returns its public key |
| `EraseIdentity` | `IDENTITY` | provision | removes the identity section; no-op when absent |

* `IdentityState`: `NOT_PROVISIONED` (no public key) or `PROVISIONED` (32-byte public key).
* `ProvisionIdentity` succeeds only when not provisioned, otherwise `IDENTITY_ALREADY_PROVISIONED`. An existing
  identity is never silently replaced; erase it explicitly first.
* A secret that is not exactly 32 bytes is `INVALID_ARGUMENT`. After storing, the device reads the identity back and
  compares the public key before answering.
* No response ever carries the secret. The request type has no `Debug` output so the secret cannot reach a log.
* Errors: `IDENTITY_NOT_PROVISIONED`, `IDENTITY_ALREADY_PROVISIONED` (plus `INVALID_ARGUMENT`, `UNAVAILABLE`,
  `INTERNAL`, `PERMISSION_DENIED`).
* The master seed, the number and the derivation path are never sent to the device.

### Security status

Provisioning moves a private key over BLE, so it must only be accepted on a protected link. **BLE pairing is not
implemented yet** (see [Control plane](control.md#security)): the Altruist Urban firmware runs `OpenPolicy`, so today
anyone in radio range can provision or erase an identity. Provision and erase use a dedicated `Privilege::Provision`,
so `RequireEncryption` / `RequireAuthenticated` already reject them on an unprotected link with `PERMISSION_DENIED`
(covered by host tests); enabling pairing and switching the firmware policy is the remaining step. Provision devices
in a trusted place until then.

## `rohi` command line

`id` is a full alias of `identity`.

```bash
rohi identity                                          # state and public key; never the secret
rohi identity provision -n 42 --seed master.suri       # -n, --number and --seed are required
rohi identity erase [--yes]                            # asks for confirmation unless --yes
```

`provision` reads the master through the same derivation as above, derives `//<number>`, sends only the derived
secret and **fails unless the public key reported by the device equals the one derived locally**. The seed file is
read on the host and never sent, stored, logged or printed; neither is the derived secret. A bad seed fails before any
device is contacted.

```text
$ rohi id
State: not provisioned
$ rohi id provision -n 42 --seed master.suri
Identity provisioned

Public key: 0x8743…f2f3
$ rohi id
State:      provisioned
Public key: 0x8743…f2f3
```

## Firmware

`altruist_urban::identity` owns access to the identity: it loads it at boot, serves the control requests through
`ConfigHandle`, keeps an `IdentitySigner` for future signed telemetry (`IdentityHandle::sign`, which returns
`NotProvisioned` when erased) and updates it on provision and erase.

The secret passes through fixed-size buffers of the configuration channel and the BLE request path, which are not
scrubbed after use.

## Validation checklist (hardware)

1. Fresh Altruist Urban: `rohi id` reports *not provisioned*; sensors and BLE diagnostics work.
2. `rohi id provision -n 42 --seed <test-master>`: host and device public keys match.
3. Reboot: `rohi id` returns the same public key. Change unrelated configuration, reboot: unchanged.
4. `rohi id erase`: state becomes *not provisioned*, signing returns `IdentityNotProvisioned`, sensors keep running.
5. Provision again with the same master and number: the original public key is restored.

## Out of scope

Automatic device-side generation, per-sensor keypairs, certificates, multiple identities, key rotation, secure
element, storing the number on the device, remote master storage, and Connectivity Protocol telemetry/uplink.
