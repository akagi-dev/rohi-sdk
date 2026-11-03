# Altruist simulator

The Altruist Urban firmware stack as a host process: the portable `altruist-urban` application (configuration store,
Wi-Fi manager, measurement runtime, control dispatcher, identity, NTP time, telemetry uplink) on Embassy's
`platform-std` executor, with simulated sensors, a file-backed NOR flash, a native Wi-Fi model, the IP stack of the host
and the ROHI control protocol over localhost TCP. No ESP toolchain and no hardware are needed.

```bash
cargo run -p altruist-sim                       # control on 127.0.0.1:9300, management on :9301
cargo run -p altruist-sim -- --scenario firmwares/altruist-sim/scenarios/office.toml
rohi --device tcp://127.0.0.1:9300 status       # the normal rohi CLI over TCP
cargo test -p altruist-sim                      # unit tests and end-to-end tests
```

It does not depend on `esp-hal`, `esp-radio`, `esp-rtos` or any `*-esp` crate. Scenarios, fault injection, storage,
several instances and the test harness are described in the book:
[Run a simulated Altruist](../../docs/guides/altruist-sim.md).

| Module | Role |
|--------|------|
| `app` | composition root: spawns the same owners and tasks as the firmware |
| `sensors` | simulated BMx280, SCD4x, SDS011 and noise drivers (`SensorDriver`) |
| `storage` | `FileFlash`, the `ConfigStorage` backend with NOR rules and fault injection |
| `wifi` | `NativeWifi`, the `WifiBackend` over simulated access points |
| `network` | `NativeIp`, the `IpBackend` over host sockets |
| `control` | `TcpTransport`, the `BleTransport` carrier of the control frames |
| `management` | the simulator management interface and the scenario timeline |
| `scenario`, `command`, `world` | the scenario format, the command language and the simulated world |
