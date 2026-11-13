# Run Altruist Urban on `std`

The Altruist Urban application also runs as a normal process on your computer. It is not a set of test mocks and not a
separate firmware: it is the same `altruist-urban` package, built from the same portable crates, on the **`std`
platform** instead of the ESP32 platform. Use it to develop and test the ROHI desktop and mobile applications without a
board, to test configuration, control and network flows end to end and to reproduce sensor, network and storage failures
on demand.

```text
                         altruist-urban
                  shared application and services
                    Config · Status · Action
                              │
                        rohi-control
                      common controller
                              │
              ┌───────────────┴───────────────┐
      rohi-control-ble                 rohi-control-tcp
        ┌─────┴──────┐                        │
     ESP BLE     BlueZ BLE               rohi-net-std
        │            │                   host TCP
     ESP32       std Altruist Urban ────────┘
                         │
                  rohi-sim (optional sensors)
```

The platform changes only what is ESP-specific. The measurement runtime, the configuration store, the Wi-Fi manager, the
control dispatcher, identity, time and the telemetry uplink are the ones the firmware runs.

| On the device | On `std` |
|---------------|----------|
| I²C / UART / I²S sensor drivers | simulated sensors (`sensor-simulator`) that feed the same measurement runtime; without the feature **no sensor is reported** |
| `rohi-cfg` flash partition | a file that follows NOR flash rules (erase before rewrite, 1→0 programming, 4 KiB sectors) |
| Wi-Fi radio | a model of access points behind the same Wi-Fi manager (`rohi-sim`) |
| `embassy-net` IP stack | the IP stack of your computer (DNS, TCP, UDP), including `localhost` (`rohi-net-std`) |
| BLE peripheral (TrouBLE) | optional BlueZ peripheral on Linux (`std-ble`, see [Linux BLE](linux-ble.md)) |
| control over BLE | control over BLE and/or TCP (`std-tcp`, `rohi-control-tcp`); the protocol is the same |

## Cargo features

| Feature | Purpose |
|---------|---------|
| `std` | build and run on a host OS; the binary `altruist-urban`; the development-management interface |
| `std-tcp` | control over TCP; requires `std` (implied) |
| `std-ble` | control over a BlueZ BLE peripheral; requires `std` (implied), Linux only |
| `sensor-simulator` | simulated BMx280, SCD4x, SDS011 and noise sensors and scenarios; requires `std` (implied) |
| `esp32c3`, `esp32c6` | the hardware targets; **cannot** be combined with `std` |

`std-tcp`, `std-ble` and `sensor-simulator` are independent: any subset builds. Combinations of the platforms are
rejected when the crate is built, and `std-ble` is rejected outside Linux. Without `sensor-simulator` no sensor is
fabricated: the `sensors` service reports every sensor as not detected. Without `std-ble` the `ble` service is
`disabled`; it never pretends to advertise over TCP.

## Run it

```bash
# Desktop development: control over TCP, simulated sensors
cargo run -p altruist-urban --bin altruist-urban --features std,std-tcp,sensor-simulator

# Mobile development on Linux: control over BLE
cargo run -p altruist-urban --bin altruist-urban --features std,std-ble,sensor-simulator
```

No ESP toolchain and no flashing tools are needed. TCP and BLE can be used together
(`--features std,std-tcp,std-ble,sensor-simulator`). The process prints a `READY` line with the interfaces that run once
it is up:

```text
READY control=127.0.0.1:9300 manage=127.0.0.1:9301 ble=default
```

Stop it with Ctrl-C or SIGTERM: it flushes its storage and exits cleanly. Startup problems (a storage file that cannot be
opened, an invalid scenario, a port that is already in use) are reported with a message and a non-zero exit status, never
with a panic.

| Option | Meaning |
|--------|---------|
| `--name` | device name used in logs |
| `--bind ADDRESS` | address of the control and management listeners (default `127.0.0.1`) |
| `--unsafe-allow-remote` | allow a non-loopback `--bind`, see [Security](#security) |
| `--control-port` | port of the TCP control transport (`std-tcp`; default 9300, `0` picks a free port) |
| `--manage-port` | port of the management interface (default: control port + 1) |
| `--ble-adapter` | the Bluetooth adapter, for example `hci0` (`std-ble`; default: the default adapter of BlueZ) |
| `--storage FILE` | configuration flash image (default `.altruist-urban/device.flash`); the persistent log is kept next to it in `device.log.flash` |
| `--ephemeral` | keep the flash and the persistent log in memory only |
| `--scenario FILE` | scenario file, see below |
| `--seed N` | seed of every random sequence of the simulation |
| `--log-level` | `error`, `warn`, `info`, `debug`, `trace` (`RUST_LOG` overrides it) |

## What it validates, and what it does not

It validates application logic, service boundaries, state machines, protocols, persistence (including interrupted
writes), network behaviour and failure handling. The BlueZ peripheral lets a real phone discover and operate it.

It does **not** simulate CPU timing, RF behaviour, the real Wi-Fi association and BLE controller behaviour, electrical
behaviour of I²C and UART, DMA, the ESP memory layout, power consumption or radio coexistence. Those stay
hardware-validation concerns, see [Hardware testing](../validation/hardware-testing.md). In the
[support matrix](../support/overview.md) this is software integration testing, not hardware validation.

## Control over TCP and the `rohi` tool

The normal `rohi` commands work unchanged; only the target differs:

```bash
rohi --device tcp://127.0.0.1:9300 status
rohi --device tcp://127.0.0.1:9300 status sensors
rohi --device tcp://127.0.0.1:9300 action wifi scan
echo 'a long password' | rohi --device tcp://127.0.0.1:9300 config wifi ssid=ROHI-LAB password=-
rohi --device tcp://127.0.0.1:9300 status wifi
```

`rohi system reboot` restarts the process in place and keeps its ports; `rohi system factory_reset` formats its flash
and does not restart (run `rohi system reboot` afterwards if a restart is required). TCP is an
independent transport of the same protobuf messages, with its own framing and no BLE concepts (no fragmentation, no
connection state faked from BLE). Each message is a record:

```text
+--------------------+------------------------------+
| length: u32 LE     | length bytes of Protobuf     |
+--------------------+------------------------------+
```

A client sends `Request` records and gets one `Response` record for each, in order. The length is checked against the
limits of the protocol (768 bytes for a request, 2048 for a response) before any payload is read; a length of 0 or above
the limit is answered once with an `InvalidArgument` error and the connection is closed, because the stream cannot be
resynchronized. A record that is not valid Protobuf is answered with `InvalidArgument` and the connection stays open. One
client is served at a time; another connection waits until the first closes. The contract and its tests are in the
rustdoc of [`rohi-control-tcp`](https://docs.rs/rohi-control-tcp).

## Security

TCP control and the management interface are **development transports**: they have no authentication or encryption, and
the control dispatcher allows everything, including identity provisioning and erase. Therefore:

- both listeners bind to `127.0.0.1` by default;
- a non-loopback `--bind` is refused unless `--unsafe-allow-remote` is given, and then anyone who can reach the port
  controls the device;
- BLE has no pairing or encryption either, on the device and on `std`; the link is reported as unencrypted and
  unauthenticated.

None of this is production-secure.

## Storage

The configuration lives in a flash image file and survives restarts. The process uses the real configuration store on it,
so the A/B slots and the commit-last recovery behave as on the device. Use `--ephemeral` for throw-away runs and
`--storage /tmp/device-a.flash` to keep several devices apart. A storage file can be used by one process at a time.

The persistent [device log](../use/desktop/logs.md) is a second image file next to it (`device.flash` is paired with
`device.log.flash`), with the geometry of the `rohi-log` partition, so `rohi logs` works against the process and the
history survives a restart. There is no SD card on the host: `logging.storage = sd` reports `storage_unavailable`.

## Scenarios

A scenario is a TOML file that describes the initial sensor values, the Wi-Fi networks and a timeline of events. It is
separate from the persistent device configuration. Three are checked in under `crates/sim/scenarios/`: `office.toml`,
`sensor-failure.toml` and `network-flap.toml`.

```toml
[sensors]
temperature = 22.5      # °C
humidity = 45.0         # %
pressure = 1013.0       # hPa
co2 = 600               # ppm
pm25 = 8                # µg/m³
pm10 = 12
noise = 42              # dB
interval = "1s"         # time between measurements
warm_up = { scd4x = "5s" }

[freshness]
stale_after = "10s"     # measurements older than this are stale

[wifi]
networks = ["ROHI-LAB", { ssid = "Office", rssi = -60, security = "wpa2", password = "secret" }]

[[events]]
at = "30s"              # uptime of the device
action = "set"
sensor = "co2"
value = 1200

[[events]]
at = "60s"
command = "fail sds011"
```

The default device has the sensors BMx280, SCD4x, SDS011 and the noise microphone, and the networks `ROHI-LAB`, `Office`
and `Guest`. Values reach the control plane through the normal measurement runtime; nothing writes a snapshot directly,
so a simulated change is visible through ordinary control operations. With `--seed N` the same scenario, seed and
configuration produce the same simulated sequence.

## Development management and fault injection

The management interface is a line-based text protocol on its own port. It is separate from the device control protocol,
is not part of `rohi-control-proto` and only concerns the simulation. Ordinary device operations always go through the
control transport.

```bash
printf 'set co2 1200\nfail scd4x\nstatus\n' | nc -N 127.0.0.1 9301
```

| Command | Effect |
|---------|--------|
| `set <value> <number>` | change `temperature`, `humidity`, `pressure` (hPa), `co2`, `pm25`, `pm10`, `noise` (needs `sensor-simulator`) |
| `fail <sensor> [unavailable\|init\|read\|timeout\|nodata]`, `recover <sensor>` | sensor faults; `nodata` is a warm-up or a stale measurement (needs `sensor-simulator`) |
| `network down`, `network up` | all access points disappear or return |
| `ap <ssid> down\|up\|rssi <dBm>` | one access point |
| `wifi fail association\|authentication\|dhcp`, `wifi recover`, `wifi delay <ms>`, `wifi drop`, `wifi flap <up ms> <down ms>\|off` | connection failures, delays, a lost or flapping link |
| `ip fail offline\|dns\|connect\|reset`, `ip recover`, `ip latency <ms>`, `ip drop-next` | faults of the IP data plane above the sockets |
| `storage power-loss program\|erase <bytes>` | the device loses power after that many bytes; the process ends like a device would |
| `storage corrupt crc\|commit` | damages the active configuration slot (seen at the next start) |
| `measurements`, `status`, `shutdown` | the measurement snapshot with freshness, the simulated world, a clean stop |

A scenario can run any of these at a given uptime. Sensor failures travel through the normal sensor lifecycle: the sensor
becomes degraded, then failed, and recovers with the usual backoff. The simulated sensors use shorter retry waits than
the firmware so that scenarios stay short.

## Several instances

Every instance needs its own storage and ports:

```bash
altruist-urban --name urban-a --control-port 9301 --manage-port 9401 --storage /tmp/urban-a.flash
altruist-urban --name urban-b --control-port 9302 --manage-port 9402 --storage /tmp/urban-b.flash
```

Instances are separate processes with no shared state. Two instances must not use the same Bluetooth adapter.

## Integration tests

```bash
cargo test -p altruist-urban --features std,std-tcp,sensor-simulator    # unit and end-to-end tests
cargo test -p altruist-urban --features std,std-tcp --test std_plain    # without simulated sensors
cargo test -p altruist-urban --features std,std-tcp,std-ble --test std_ble
cargo test -p rohi-net-std --features ble                                # BLE bridge, no adapter needed
```

The end-to-end tests start the process on free ports with its own storage, wait for the `READY` line (no sleeps), talk to
it with the real `rohi` client library and stop it. They cover startup, sensors and freshness, sensor failure and recovery,
configuration persistence, recovery from an interrupted write, the Wi-Fi manager (connect, back off, recover), an
abandoned request, TCP framing errors, reconnection, readiness and the refusal of a remote bind. The harness is in
`firmwares/altruist-urban/tests/common`. None of them needs an ESP board or a Bluetooth adapter.

## Local edge end-to-end

Identity, time and the telemetry uplink run on the host network. Provisioning a test identity and sending telemetry to a
local `robins edge` is possible by hand (`rohi action identity provision`, then
`rohi config connectivity endpoint=http://127.0.0.1:PORT`), but an automated end-to-end test against a local edge is **not
implemented yet**. There is no TLS yet.
