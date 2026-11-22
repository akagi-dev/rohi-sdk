# Meshtastic radio over UART

A device can use a local **Meshtastic radio** in two ways, both opt-in with the `meshtastic-uart` feature of
`altruist-urban` (`cargo check-esp32c3 --features meshtastic-uart`, the same for `esp32c6`):

* the **`meshtastic` service** manages and diagnoses the radio: its configuration, its health and a small send/receive
  test;
* the **`connectivity` service** delivers sealed telemetry packets to `meshtastic+uart://!xxxxxxxx` endpoints through
  that radio, as a primary or a fallback path of a route.

The radio is a **separate Meshtastic device** running Meshtastic firmware, not a bare LoRa chip. Its Serial module must
speak the protobuf PhoneAPI (`PROTO` mode) at the baud rate of the `meshtastic` section. Which UART and which pins carry it
is decided by the board (`rohi-hal-esp`), never by the configuration or by an endpoint.

| Chip | UART0 TX | UART0 RX | Status |
|------|----------|----------|--------|
| ESP32-C3 | GPIO21 | GPIO20 | chip defaults, **not verified** against the board revision |
| ESP32-C6 | GPIO22 | GPIO20 | the pins the CN4 documentation reserves for the Meshtastic module; the direction is **unverified**, swap them if the radio stays silent |

UART1 stays with the SDS011. UART0 is also the boot console of the chip, so the firmware sends its log over USB
Serial/JTAG (`esp-println` feature `jtag-serial`); the ROM bootloader still prints on UART0 before the application starts.
Use 3.3 V logic and a common ground.

## Configuration

```sh
rohi config meshtastic                 # baud_rate = 115200
rohi config meshtastic baud_rate=57600 # applies by starting a new session, no reboot
```

`baud_rate` is the only field: 9600, 19200, 38400, 57600, 115200 or 230400. There is no `enabled` field; the service runs
whenever the backend is built in.

## Status and issues

```sh
rohi status meshtastic
```

| Field | Meaning |
|-------|---------|
| `node_id`, `firmware_version`, `hardware_model` | what the radio said about itself; absent until known |
| `last_rx_age_s`, `session_age_s` | seconds since the last valid message and since the session was established |
| `tx_frames`, `rx_frames` | messages written to the UART and valid messages received |
| `framing_errors`, `decode_errors` | invalid frame headers and frames that did not decode |
| `reconnects` | session attempts after the first |
| `last_error` | `none`, `uart_io`, `uart_closed`, `handshake_failed`, `radio_rebooted`, `framing`, `decode`, `requested`, `baud` |

The counters live in RAM, restart at zero with the device and saturate at the largest `u64`. Health is `unknown` until the
first synchronization finished or failed, `ok` while the session is up, `degraded` while it is recovered, `error` when the UART
cannot be used or the radio does not answer. The service is not essential: a missing radio does not make the device
unhealthy. Issues: `uart_unavailable`, `radio_not_configured` (the radio talks, but not the PhoneAPI: set the Serial module to
`PROTO`), `radio_unresponsive`, `protocol_error`, `radio_reconnecting`.

## Actions

```sh
rohi action meshtastic send to=!deadbeef text="Hello ROHI"   # TEXT_MESSAGE_APP, unicast, channel 0, want_ack
rohi action meshtastic inbox                                   # six entries, newest first
rohi action meshtastic reconnect                               # new session without a reboot
```

`send` returns the `packet_id` once the **local radio accepted the packet**. That is not a delivery: the routing result
arrives later and shows up in `inbox` as an `ack` entry. `inbox` keeps the last six text messages and acknowledgements
of diagnostic sends in RAM, does not clear on reading, skips texts over 64 bytes and never shows telemetry traffic.

## Delivering telemetry

```sh
rohi connectivity set_endpoint 0 0 meshtastic+uart://!deadbeef
rohi connectivity set_fallback 0 http://ingress.example.org
```

The URI is exactly `meshtastic+uart://!` and eight hexadecimal digits: the **gateway** node, not the local radio. No port,
path, query or `device` parameter exists. A build without the UART backend refuses the scheme (`unsupported_endpoint_scheme`),
and `meshtastic+ble://` is refused everywhere: there is no BLE backend. A route may mix transports; the sealed packet is
never rebuilt, re-signed or changed.

The framing is Meshtastic Transport v1 (219-byte MTU, at most 16 fragments and 3376 bytes), every fragment a PKI-encrypted
unicast with a routing acknowledgement request. The delivery succeeds when **every** fragment got its matching successful
routing ACK. That proves the mesh delivered the fragments, **not** that the gateway reassembled, validated or published
them. A packet too large for the mesh is not an error of the route: it is offered to the other path, and only a packet no path
can carry is dropped and counted. Delivery is at-least-once; the gateway deduplicates by the signed envelope.

If a firmware without the backend reads a stored route that uses it, the stored data stays untouched, `connectivity`
reports `unsupported_scheme` and sends nothing until the route is removed or replaced (`set_endpoint 0 0 ""`).

Reference: [`rohi-meshtastic`](https://docs.rs/rohi-meshtastic) and
[`rohi-connectivity-meshtastic`](https://docs.rs/rohi-connectivity-meshtastic). The wiring is in
`firmwares/altruist-urban/src/{meshtastic,connectivity}.rs` and `src/esp/meshtastic.rs`.
