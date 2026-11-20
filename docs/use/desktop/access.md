# Access and the TCP key

A device decides what a client may do. Over TCP the **access mode** says what a client without the device's 32-byte
**pre-shared key** (PSK) may do; a client that completes the handshake with the key may do everything, and everything it
sends and receives is authenticated and encrypted. **BLE is open in the current build**: no pairing, no link encryption, and
every client in radio range may do everything, including changing the configuration, provisioning the identity, resetting
the device and asking for the TCP key.

There is one public service, `control`, and one config section, `control`, for both endpoints.

| `tcp_access_mode` | Without the key | With the key |
|-------------------|-----------------|--------------|
| `open` | read, change and provision, in plaintext | everything |
| `protected` (default) | read only, in plaintext | everything |
| `private` | nothing: a plaintext client is not served | everything |

| Transport | What it means |
|-----------|---------------|
| TCP | the mode above, set with `tcp_access_mode`; the handshake with the PSK unlocks everything, every record is authenticated and encrypted |
| BLE | **open**: no pairing, no encryption, no setting to change it; the `control` status raises the `unsecured` issue while it runs |

The mode is about *who may do what*. Whether a secret may travel is a separate rule: the action that generates the key
(`tcp_generate_psk`) runs only on an authenticated and encrypted connection of the current key, in every mode, so `open`
never makes the key travel in plaintext TCP. (Open BLE is the accepted exception of this phase.)

Open BLE is a deliberate, temporary, insecure state. Protecting TCP does not secure a device whose BLE is open. See
[Access control](../../build/control-protocol.md#access-control) for the protocol, and what it does and does not give: in
particular **there is no forward secrecy**, so whoever learns the key later can decrypt a recorded session.

## Get the key from the device

The device generates the key; a client never chooses it.

```bash
rohi -D ble://ALTRUIST-URBAN-A12F action run control tcp_generate_psk
# PSK                                                               FINGERPRINT
# 0bca27b51a6f36f22bf416601de43452335375770144ba6e69475271216ef5df  0ca29e40355a303b
```

`control tcp_generate_psk` is an ordinary action. **Every call makes a new key**, stores it, ends the TCP connections of the
old one and returns the new key in the result; there is no action that reads the current key, and no way to set a key from a
client.

**The generic action output prints the key.** `rohi` has no special handling for this action: the `PSK` column of the result
is the key itself, in clear text on your terminal, in your scrollback and in whatever captures the output (a pipe, a CI log,
`--output json`). Run it where nobody can see the screen, do not log it, and save it straight into a file only you can read:

```bash
(umask 077; rohi -D ble://ALTRUIST-URBAN-A12F action run control tcp_generate_psk | awk 'NR==2 {print $1}' > tcp.key)
```

The `FINGERPRINT` is not a secret: it tells you which key a device holds. Over BLE the key also travels in the clear: run the
action where nobody else is in radio range.

The key file holds 64 hex characters; keep it secret. `rohi` refuses a key file that other users can read, and the key is
never a command line argument. Setting `ROHI_PSK_FILE` is the same as passing `--psk-file`. The status of `control` does not
show the key, whether one exists, or the mode.

## Use a protected or private endpoint

Without a key `rohi` sends plaintext requests: a `protected` endpoint answers reads (`status`, `config` without changes,
`info`) and refuses changes. With the key, `rohi` does the handshake and encrypts every request, which is how you change or
provision a `protected` endpoint and the only way into a `private` one:

```bash
rohi -D tcp://192.168.1.50:9300 status control                                  # plaintext read, protected endpoint
rohi -D tcp://192.168.1.50:9300 --psk-file tcp.key config wireless wifi_ssid=ROHI-LAB
rohi -D tcp://192.168.1.50:9300 --psk-file old.key action run control tcp_generate_psk   # rotate (see below)
```

With another key the connection is refused: a device that does not know your key cannot complete the handshake and `rohi`
does not trust it, and a client that failed the handshake is never served in plaintext. If a request is refused:

| Reason | Meaning |
|--------|---------|
| `no_psk` | no key is provisioned, so nobody can authenticate to the TCP endpoint; get one with `rohi action run control tcp_generate_psk` (over BLE) |
| `encryption_required` | the request needs the authenticated link; connect with `--psk-file` |

**Rotation.** To replace the key, connect with the **current** key (`--psk-file`) and run `rohi action run control tcp_generate_psk`
(save its result as above), or run it over BLE. The device ends the TCP connections that used the old key, and a request of
such a connection that was still waiting for the device is not run at all: reconnect with the new key. A TCP session that is
interrupted by the change is not a failure of the request.

**One slow client does not block the endpoint.** The device serves one TCP client at a time. A client has 10 seconds in
total to send its preamble and finish the handshake and 10 seconds in total for each request record, however slowly it sends
the bytes; after that the connection is closed. A client that sends nothing at all after a record is closed after 60 seconds.

## Settings and status

```bash
rohi config control                          # tcp_access_mode, tcp_bind_address, tcp_port
rohi status control                          # ble_sessions, tcp_sessions, tcp_state, requests, errors; health and issues
```

There is no `enabled` setting: a service that is part of the firmware always starts, with the defaults and your stored
values on top, and a failure to start shows as `failed` in `tcp_state` and as the `endpoint_failed` issue. The three settings
apply together at the next start (reboot); a saved setting is never partly applied. `rohi config` tells you when a reboot is
needed (`restart_required`). A factory reset restores the defaults and removes the key. Only a device with a TCP endpoint (the
`std` platform) has the `control` config section.

`requests` counts the requests the device handled since boot, including those refused by the access mode; `errors` counts
failures (a broken record, an I/O error, a handshake that did not authenticate), each once. A refused request is not an
error. The health is `ok` when every endpoint the device has works, `degraded` when one failed and another works, and `error`
when none works; the mode, the key and the number of sessions do not change it.

## Recovery

If the key is lost: run `rohi action run control tcp_generate_psk` over BLE (and save the result), or `rohi system factory_reset`
(it also removes the configuration and the identity). On the `std` device, `--control-psk-file FILE` gives the process a key
for one run (it replaces the stored one and is never stored), `--control-mode open|protected|private` (or `--control-open`)
sets the access mode for one run (open is plaintext, loopback only).
