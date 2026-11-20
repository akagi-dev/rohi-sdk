# Access and the TCP key

A device decides, per transport, what a client may do. Over TCP a **restricted** endpoint (the default) serves only clients
that know the device's 32-byte **pre-shared key** (PSK), and everything that client sends and receives is authenticated and
encrypted. **BLE is open in the current build**: no pairing, no link encryption, and every client in radio range may do
everything, including changing the configuration, provisioning the identity, resetting the device and asking for the TCP key.

| Transport | Default | What it means |
|-----------|---------|---------------|
| TCP (`control_tcp`) | **restricted** | the client completes the handshake with the PSK; every record is authenticated and encrypted. `access_mode=open` is plaintext and unauthenticated, for a loopback development run |
| BLE (`control_ble`) | **open** | no pairing, no encryption, no setting to change it; the status of `control_ble` raises the `open` issue |

Open BLE is a deliberate, temporary, insecure state. Restricting TCP does not secure a device whose BLE is open. See
[Access control](../../build/control-protocol.md#access-control) for the protocol, and what it does and does not give: in
particular **there is no forward secrecy**, so whoever learns the key later can decrypt a recorded session.

## Get the key from the device

The device generates the key; a client never chooses it.

```bash
rohi -D ble://ALTRUIST-URBAN-A12F control_tcp reveal_psk
# PSK                                                               FINGERPRINT
# 0bca27b51a6f36f22bf416601de43452335375770144ba6e69475271216ef5df  0ca29e40355a303b
```

`control_tcp reveal_psk` is an ordinary action (`rohi action control_tcp reveal_psk` is the same). **Every call makes a new
key**, stores it, ends the TCP connections of the old one and returns the new key in the result; there is no action that
reads the current key, and no way to set a key from a client.

**The generic action output prints the key.** `rohi` has no special handling for this action: the `PSK` column of the result
is the key itself, in clear text on your terminal, in your scrollback and in whatever captures the output (a pipe, a CI log,
`--output json`). Run it where nobody can see the screen, do not log it, and save it straight into a file only you can read:

```bash
(umask 077; rohi -D ble://ALTRUIST-URBAN-A12F control_tcp reveal_psk | awk 'NR==2 {print $1}' > tcp.key)
```

The `FINGERPRINT` is not a secret: it tells you which key a device holds. Over BLE the key also travels in the clear: run the
action where nobody else is in radio range.

The key file holds 64 hex characters; keep it secret. `rohi` refuses a key file that other users can read, and the key is
never a command line argument. Setting `ROHI_PSK_FILE` is the same as passing `--psk-file`. The status of `control_tcp`
says only whether a key is provisioned (`psk_enrolled`).

## Use a restricted endpoint

With the key, `rohi` does the handshake and encrypts every request:

```bash
rohi -D tcp://192.168.1.50:9300 --psk-file tcp.key config wifi ssid=ROHI-LAB
rohi -D tcp://192.168.1.50:9300 --psk-file old.key control_tcp reveal_psk   # rotate (see below)
```

Without the key (or with another one) the connection is refused: a restricted endpoint answers nothing in the clear, a device
that does not know your key cannot complete the handshake and `rohi` does not trust it. If a request is refused:

| Reason | Meaning |
|--------|---------|
| `no_psk` | no key is provisioned, so a restricted TCP endpoint serves nobody; get one with `rohi control_tcp reveal_psk` (over BLE) |
| `encryption_required` | the action needs a protected link; connect to a restricted endpoint with `--psk-file` |

**Rotation.** To replace the key, connect with the **current** key (`--psk-file`) and run `rohi control_tcp reveal_psk` (save its
result as above), or run it over BLE. The device ends the TCP connections that used the old key, and a request of such a
connection that was still waiting for the device is not run at all: reconnect with the new key. A TCP session that is
interrupted by the change is not a failure of the request.

**One slow client does not block the endpoint.** The device serves one TCP client at a time. A client has 10 seconds in
total to finish the handshake and 10 seconds in total for each request record, however slowly it sends the bytes; after
that the connection is closed. A client that sends nothing at all is closed after 60 seconds.

## Settings and status

```bash
rohi config control_tcp                      # access_mode, bind_address, port
rohi status control_tcp                      # state, access_mode and port in use, sessions, accepted and denied requests, auth_failures, psk_enrolled
rohi status control_ble                      # state, sessions, accepted and denied requests
```

There is no `enabled` setting: a service that is part of the firmware always starts, with the defaults and your stored
values on top, and a failure to start shows as `failed` in its status. The settings of `control_tcp` apply as a whole at
the next start (reboot); a saved setting is never partly applied, and the status shows only what is running. `rohi config`
tells you when a reboot is needed (`restart_required`). A factory reset restores the defaults and removes the key.

## Recovery

If the key is lost: run `rohi control_tcp reveal_psk` over BLE (and save the result), or `rohi system factory_reset` (it also removes the
configuration and the identity). On the `std` device, `--control-psk-file FILE` gives the process a key for one run
(it replaces the stored one and is never stored), or `--control-open` opens the endpoint for one run (plaintext, loopback only).
