# Access and the TCP key

A device decides, per transport, what a client may do. Over TCP the answer is simple: a **restricted** endpoint (the default)
serves only clients that know the device's 32-byte **pre-shared key** (PSK), and everything that client sends and receives is
authenticated and encrypted. Over BLE a client may read, and changes need a paired (encrypted) link when the endpoint is
restricted.

| Transport | Default | `restricted` means |
|-----------|---------|--------------------|
| TCP (`control_tcp`) | **restricted** | the client completes the handshake with the PSK; every record is authenticated and encrypted |
| BLE (`control_ble`) | **open** | the link must be paired (encrypted) before a change |

`open` is deliberately insecure: on TCP it is plaintext and unauthenticated (for a loopback development run), on BLE anyone in
radio range can change the configuration. Restricting TCP does not secure a device whose BLE is open. The key itself is never
accepted over an unprotected link, in any mode. See [Access control](../../build/control-protocol.md#access-control) for the
protocol, and what it does and does not give: in particular **there is no forward secrecy**, so whoever learns the key later
can decrypt a recorded session.

## Create a key and give it to the device

```bash
rohi tcp-key generate tcp.key                                  # a new random key, readable by you only
rohi -D ble://ALTRUIST-URBAN-A12F --psk-file tcp.key tcp-key set    # over a paired BLE link
```

The key file holds 64 hex characters; keep it secret and back it up, because a lost key can only be replaced over a paired
BLE link (or by a factory reset). `rohi` refuses a key file that other users can read, and the key is never a command line
argument. Setting `ROHI_PSK_FILE` is the same as passing `--psk-file`. The device never shows the key: the status of
`control_psk` says only whether one is provisioned.

`tcp-key set` needs a protected link. Over an unpaired BLE link or any unauthenticated TCP link the device answers
`pairing_required` or `encryption_required` and nothing changes.

## Use a restricted endpoint

With the key, `rohi` does the handshake and encrypts every request:

```bash
rohi -D tcp://192.168.1.50:9300 --psk-file tcp.key config wifi ssid=ROHI-LAB
rohi -D tcp://192.168.1.50:9300 --psk-file old.key tcp-key set --new-key new.key   # rotate (see below)
rohi tcp-key clear                                                # over a paired BLE link: forget the key
```

Without the key (or with another one) the connection is refused: a restricted endpoint answers nothing in the clear, a device
that does not know your key cannot complete the handshake and `rohi` does not trust it. If a request is refused:

| Reason | Meaning |
|--------|---------|
| `no_psk` | no key is provisioned, so a restricted TCP endpoint serves nobody; provision one over a paired BLE link |
| `encryption_required` | the action needs a protected link; connect to a restricted endpoint with `--psk-file` |
| `pairing_required` | a restricted BLE endpoint (or a secret) needs a paired link |

**Rotation.** To replace the key, connect with the **current** key (`--psk-file`) and run `rohi tcp-key set --new-key FILE` with the **new** key
(the key travels inside the encrypted channel), or do it over paired BLE. The device ends the TCP connections that
used the old key; reconnect with the new one. A TCP session that is interrupted by the change is not a failure of the request.

## Change the modes

```bash
rohi config control_tcp                      # enabled, access_mode, bind_address, port
rohi config control_ble access_mode=restricted
rohi status control_tcp                      # state, sessions, accepted and denied requests, auth_failures, pending changes
```

The access mode applies at once. `enabled`, `bind_address` and `port` of `control_tcp` apply at the next start and the
status shows `restart_required`. A factory reset restores the defaults and removes the key.

## Recovery

If the key is lost: over a paired BLE link run `rohi tcp-key set` with a new key, or `rohi system factory_reset` (it also removes
the configuration and the identity). On the `std` device, `--control-psk-file FILE` gives the process a key for one run
(it replaces the stored one and is never stored), or `--control-open` opens the endpoint for one run (plaintext, loopback only).
