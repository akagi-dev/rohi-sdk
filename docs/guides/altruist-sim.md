# Run a simulated Altruist (`altruist-sim`)

`altruist-sim` is the Altruist Urban firmware stack running as a normal process on your computer. It is not a set of test
mocks: it is the same application, built from the same portable crates, on a **native platform** instead of the ESP32
platform. Use it to develop without hardware, to test configuration, control and network flows end to end and to
reproduce sensor, network and storage failures on demand.

```text
                    portable ROHI SDK
                           │
                ┌──────────┴──────────┐
                ▼                     ▼
            ESP platform         native platform
                │                     │
        altruist-urban          altruist-sim
```

The simulator is a platform implementation, not a different application or protocol stack: the measurement runtime, the
configuration store, the Wi-Fi manager, the control dispatcher, identity, time and the telemetry uplink are the ones the
firmware runs. Only what is ESP-specific is replaced by a native backend:

| On the device | In the simulator |
|---------------|------------------|
| I²C / UART / I²S sensor drivers | simulated sensors that feed the same measurement runtime |
| `rohi-cfg` flash partition | a file that follows NOR flash rules (erase before rewrite, 1→0 programming, 4 KiB sectors) |
| Wi-Fi radio | a model of access points behind the same Wi-Fi manager |
| `embassy-net` IP stack | the IP stack of your computer (DNS, TCP, UDP), including `localhost` |
| BLE link | TCP on `127.0.0.1` carrying the same control frames |

## What it validates, and what it does not

It validates application logic, service boundaries, state machines, protocols, persistence (including interrupted
writes), network behaviour and failure handling.

It does **not** simulate CPU timing, RF behaviour, the real Wi-Fi association and BLE protocols, electrical behaviour of
I²C and UART, DMA, the ESP memory layout, power consumption or radio coexistence. Those stay hardware-validation
concerns, see [Hardware testing](../validation/hardware-testing.md). In the [support matrix](../support/overview.md) the
simulator does not count as peripheral-level simulation.

## Run it

```bash
cargo run -p altruist-sim
```

No ESP toolchain and no flashing tools are needed. The simulator prints a `READY` line with its addresses once it is
up:

```text
READY control=127.0.0.1:9300 manage=127.0.0.1:9301
```

Stop it with Ctrl-C or SIGTERM: it flushes its storage and exits cleanly. Startup problems (a storage file that cannot be
opened, an invalid scenario, a port that is already in use) are reported with a message and a non-zero exit status, never
with a panic.

| Option | Meaning |
|--------|---------|
| `--name` | device name used in logs |
| `--control-port` | port of the control transport (default 9300, `0` picks a free port) |
| `--manage-port` | port of the simulator management interface (default: control port + 1) |
| `--storage FILE` | configuration flash image (default `.altruist-sim/device.flash`) |
| `--ephemeral` | keep the flash in memory only |
| `--scenario FILE` | scenario file, see below |
| `--seed N` | seed of every random sequence of the simulation |
| `--log-level` | `error`, `warn`, `info`, `debug`, `trace` (`RUST_LOG` overrides it) |

## Use the `rohi` tool with it

The normal `rohi` commands work unchanged; only the target differs:

```bash
rohi --device tcp://127.0.0.1:9300 status
rohi --device tcp://127.0.0.1:9300 status sensors
rohi --device tcp://127.0.0.1:9300 action wifi scan
echo 'a long password' | rohi --device tcp://127.0.0.1:9300 config wifi ssid=ROHI-LAB password=-
rohi --device tcp://127.0.0.1:9300 status wifi
```

`rohi reboot` restarts the simulator in place and keeps its ports; `rohi factory-reset` formats its flash. The control
transport carries the same protobuf requests as BLE (one length-prefixed BLE frame per TCP record) and reaches the same
dispatcher, so a request behaves the same on a real Altruist over BLE and on the simulator. As on BLE, one client is served
at a time and there is no link security yet.

## Storage

The configuration lives in a flash image file and survives restarts. The simulator uses the real configuration store on
it, so the A/B slots and the commit-last recovery behave as on the device. Use `--ephemeral` for throw-away runs and
`--storage /tmp/device-a.flash` to keep several devices apart. A storage file can be used by one simulator at a time.

## Scenarios

A scenario is a TOML file that describes the initial sensor values, the Wi-Fi networks and a timeline of events. It is
separate from the persistent device configuration. Three are checked in under `firmwares/altruist-sim/scenarios/`:
`office.toml`, `sensor-failure.toml` and `network-flap.toml`.

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
at = "30s"              # uptime of the simulator
action = "set"
sensor = "co2"
value = 1200

[[events]]
at = "60s"
command = "fail sds011"
```

The default device has the sensors BMx280, SCD4x, SDS011 and the noise microphone, and the networks `ROHI-LAB`, `Office`
and `Guest`. Values reach the control plane through the normal measurement runtime; the simulator never writes a
snapshot directly. With `--seed N` the same scenario, seed and configuration produce the same simulated sequence.

## Fault injection

The management interface is a line-based text protocol on its own port. It is separate from the device control protocol
and only concerns the simulation:

```bash
printf 'set co2 1200\nfail scd4x\nstatus\n' | nc -N 127.0.0.1 9301
```

| Command | Effect |
|---------|--------|
| `set <value> <number>` | change `temperature`, `humidity`, `pressure` (hPa), `co2`, `pm25`, `pm10`, `noise` |
| `fail <sensor> [unavailable\|init\|read\|timeout\|nodata]`, `recover <sensor>` | sensor faults; `nodata` is a warm-up or a stale measurement |
| `network down`, `network up` | all access points disappear or return |
| `ap <ssid> down\|up\|rssi <dBm>` | one access point |
| `wifi fail association\|authentication\|dhcp`, `wifi recover`, `wifi delay <ms>`, `wifi drop`, `wifi flap <up ms> <down ms>\|off` | connection failures, delays, a lost or flapping link |
| `ip fail offline\|dns\|connect\|reset`, `ip recover`, `ip latency <ms>`, `ip drop-next` | faults of the IP data plane above the sockets |
| `storage power-loss program\|erase <bytes>` | the device loses power after that many bytes; the process ends like a device would |
| `storage corrupt crc\|commit` | damages the active configuration slot (seen at the next start) |
| `measurements`, `status`, `shutdown` | the measurement snapshot with freshness, the simulated world, a clean stop |

A scenario can run any of these at a given uptime. Sensor failures travel through the normal sensor lifecycle: the sensor
becomes degraded, then failed, and recovers with the usual backoff. The simulator uses shorter retry waits than the
firmware so that scenarios stay short.

## Several instances

Every instance needs its own storage and ports:

```bash
altruist-sim --name urban-a --control-port 9301 --manage-port 9401 --storage /tmp/urban-a.flash
altruist-sim --name urban-b --control-port 9302 --manage-port 9402 --storage /tmp/urban-b.flash
```

## Integration tests

`cargo test -p altruist-sim` runs end-to-end tests: each one starts a simulator process on free ports with its own
storage, waits for the `READY` line (no sleeps), talks to it with the real `rohi` client library and stops it. They cover
startup, sensors and freshness, sensor failure and recovery, configuration persistence, recovery from an interrupted
write, the Wi-Fi manager (connect, back off, recover) and an abandoned request. The harness is in
`firmwares/altruist-sim/tests/common`.

## Local edge end-to-end

The simulator is built so that identity, time and the telemetry uplink run on the host network. Provisioning a test
identity and sending telemetry to a local `robins edge` is possible by hand (`rohi action identity provision`, then
`rohi config connectivity endpoint=http://127.0.0.1:PORT`), but an automated end-to-end test against a local edge is **not
implemented yet**. There is no TLS yet.
