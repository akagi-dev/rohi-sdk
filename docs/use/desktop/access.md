# Access and the administrator key

A device decides, per transport, what an unauthenticated client may do. Reading is always open: status, configuration and
the list of actions. Changing the device (configuration, reboot, factory reset) and provisioning it (identity,
administrator key) need proof when the transport is **restricted**.

| Transport | Default | `restricted` means |
|-----------|---------|--------------------|
| TCP (`control_tcp`) | **restricted** | every change is signed with the administrator key |
| BLE (`control_ble`) | **open** | the link must be paired (encrypted) before a change |

`open` is deliberately insecure for changes: anyone who can reach that transport can reconfigure the device, provision
its identity and enroll the administrator key. Restricting TCP does not secure a device whose BLE is open. See
[Access control](../../build/control-protocol.md#access-control) for what a signature does and does not give.

## Create a key and enroll it

```bash
rohi admin keygen admin.key                      # a new key, readable by you only; prints the public key
rohi -D ble://ALTRUIST-URBAN-A12F --admin-key admin.key admin enroll     # over open BLE, no signature needed
rohi --admin-key admin.key admin public-key
```

The key file holds 64 hex characters; keep it secret and back it up, because a lost key can only be replaced through an
open or paired endpoint. `rohi` refuses a key file that other users can read, and the key is never a command line
argument. Setting `ROHI_ADMIN_KEY` is the same as passing `--admin-key`.

## Use a restricted endpoint

With the key, `rohi` signs automatically what the device refuses unsigned:

```bash
rohi -D tcp://192.168.1.50:9300 --admin-key admin.key config wifi ssid=ROHI-LAB
rohi -D tcp://192.168.1.50:9300 --admin-key admin.key admin clear      # make the device forget the key
```

Without the key such a change fails with `auth_required`; reads keep working. If a signed change is refused:

| Reason | Meaning |
|--------|---------|
| `auth_required` | the endpoint is restricted; pass `--admin-key FILE` |
| `no_admin_key` | no key is enrolled, so a restricted TCP endpoint refuses every change; enroll one over an open endpoint |
| `bad_signature` | the device trusts another key, or the connection sent too many bad signatures |
| `no_challenge` | the key was replaced or removed meanwhile; try again |
| `pairing_required` | a restricted BLE endpoint needs a paired link |

`rohi` never repeats a signed change whose answer was lost: the outcome is unknown, so read the device before trying again.

## Change the modes

```bash
rohi config control_tcp                      # enabled, access_mode, bind_address, port
rohi config control_ble access_mode=restricted
rohi status control_tcp                      # state, sessions, accepted and denied requests, pending changes
```

The access mode applies at once. `enabled`, `bind_address` and `port` of `control_tcp` apply at the next start and the
status shows `restart_required`. A factory reset restores the defaults and removes the key.

## Recovery

If the only key is lost: over an endpoint that is open or paired run `rohi system factory_reset` (it also removes the
configuration and the identity), or `rohi admin clear` there and enroll a new key. On the `std` device, start it with
`--control-open` for one run.
