///////////////////////////////////////////////////////////////////////////////
//
//  Copyright 2025-2026 Akagi Engineering <admin@akagi.dev>
//
//  Licensed under the Apache License, Version 2.0 (the "License");
//  you may not use this file except in compliance with the License.
//  You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.
//
//! End-to-end: a frame produced by `rohi-connectivity` is accepted by the
//! reference ingress (`airalab/robins`, `edge gateway`).
//!
//! ```text
//! Rohi snapshot -> core.v1.Message -> SignedEnvelope -> POST /v1/telemetry
//!               -> robins verifies the signature and accepts it
//! ```
//!
//! Opt-in: the test needs the `edge` binary of `airalab/robins`, whose
//! submodule must point at the protocol (robins `a6c1d9c` does). Build it
//! and run:
//!
//! ```text
//! ROHI_ROBINS_EDGE=/path/to/robins/target/debug/edge \
//!     cargo test -p rohi-connectivity --test robins_e2e -- --ignored
//! ```

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embedded_hal_async::delay::DelayNs;
use embedded_io_async::{ErrorType, Read, Write};
use embedded_nal_async::AddrType;
use rohi_connectivity::{
    ClockError, ConnectivityService, ConnectivityShared, Endpoint, HttpTransport, NONCE_LEN,
    NonceError, NonceSource, ServiceConfig, SnapshotSource, UtcClock,
};
use rohi_hal::runtime::Clock;
use rohi_hal::{
    Bmx280Update, Celsius, Decibels, MeasurementStore, MeasurementUpdate, MeasurementsSnapshot,
    MicrogramsPerCubicMeter, MonotonicTimestamp, NoiseUpdate, Pascal, RelativeHumidity,
    Sds011Update, SensorEvent,
};
use rohi_identity::MockSigner;
use rohi_net::{IpBackend, NetError, NetworkHandle, UdpSocket};

/// Blocking `std::net` TCP, enough for a host test of the HTTP transport.
struct StdIp;

struct StdConnection(TcpStream);

impl ErrorType for StdConnection {
    type Error = NetError;
}

impl Read for StdConnection {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, NetError> {
        self.0.read(buf).map_err(|_| NetError::ConnectionReset)
    }
}

impl Write for StdConnection {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, NetError> {
        self.0.write(buf).map_err(|_| NetError::ConnectionReset)
    }

    async fn flush(&mut self) -> Result<(), NetError> {
        self.0.flush().map_err(|_| NetError::ConnectionReset)
    }
}

struct NoUdp;

impl UdpSocket for NoUdp {
    async fn send_to(&self, _: &[u8], _: SocketAddr) -> Result<(), NetError> {
        Err(NetError::NoSocketAvailable)
    }

    async fn recv_from(&self, _: &mut [u8]) -> Result<(usize, SocketAddr), NetError> {
        Err(NetError::NoSocketAvailable)
    }
}

impl IpBackend for StdIp {
    type Connection<'a> = StdConnection;
    type Udp<'a> = NoUdp;

    fn is_ready(&self) -> bool {
        true
    }

    async fn resolve(&self, _: &str, _: AddrType) -> Result<core::net::IpAddr, NetError> {
        Err(NetError::DnsFailed)
    }

    async fn connect(&self, remote: SocketAddr) -> Result<StdConnection, NetError> {
        let stream = TcpStream::connect_timeout(&remote, Duration::from_secs(5))
            .map_err(|_| NetError::ConnectFailed)?;
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(|_| NetError::Backend)?;
        Ok(StdConnection(stream))
    }

    fn udp_socket(&self) -> Result<NoUdp, NetError> {
        Err(NetError::NoSocketAvailable)
    }
}

struct SystemUtc;

impl UtcClock for SystemUtc {
    fn now_unix_ms(&self) -> Result<u64, ClockError> {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ClockError)?;
        u64::try_from(elapsed.as_millis()).map_err(|_| ClockError)
    }
}

struct Uptime(Instant);

impl Clock for &Uptime {
    fn now(&self) -> MonotonicTimestamp {
        MonotonicTimestamp::from_millis(self.0.elapsed().as_millis() as u64)
    }
}

/// Unique nonces from the system entropy.
struct OsNonce;

impl NonceSource for OsNonce {
    fn fill_nonce(&mut self, dst: &mut [u8; NONCE_LEN]) -> Result<(), NonceError> {
        std::fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(dst))
            .map_err(|_| NonceError)
    }
}

struct Fixed(MeasurementsSnapshot);

impl SnapshotSource for Fixed {
    async fn snapshot(&self) -> Option<MeasurementsSnapshot> {
        Some(self.0)
    }
}

struct StdDelay;

impl DelayNs for StdDelay {
    async fn delay_ns(&mut self, ns: u32) {
        std::thread::sleep(Duration::from_nanos(u64::from(ns)));
    }
}

/// The reference gateway; killed on drop.
struct Gateway {
    child: Child,
    port: u16,
}

impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn start_gateway(edge: &str) -> Gateway {
    let port = free_port();
    let config = format!(
        "[http]\nenabled = true\nlisten = \"127.0.0.1:{port}\"\n\n\
         [auth]\nmode = \"none\"\n\n\
         [pubsub]\nenabled = false\n\n\
         [metrics]\nlisten = \"127.0.0.1:{}\"\n",
        free_port()
    );
    let path: PathBuf = std::env::temp_dir().join(format!("rohi-robins-e2e-{port}.toml"));
    std::fs::write(&path, config).unwrap();
    let child = Command::new(edge)
        .args(["gateway", "--config"])
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the robins `edge gateway`");
    let gateway = Gateway { child, port };
    let deadline = Instant::now() + Duration::from_secs(30);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "gateway did not start");
        std::thread::sleep(Duration::from_millis(100));
    }
    gateway
}

fn snapshot() -> MeasurementsSnapshot {
    let at = MonotonicTimestamp::from_millis(900);
    let mut store = MeasurementStore::new();
    store.apply(SensorEvent::Measurements(MeasurementUpdate::Bmx280(
        Bmx280Update::bme280(Celsius(21.5), RelativeHumidity(44.0), Pascal(101_325.0), at),
    )));
    store.apply(SensorEvent::Measurements(MeasurementUpdate::Sds011(
        Sds011Update {
            pm25: MicrogramsPerCubicMeter(12.3),
            pm10: MicrogramsPerCubicMeter(20.1),
            timestamp: at,
        },
    )));
    store.apply(SensorEvent::Measurements(MeasurementUpdate::Microphone(
        NoiseUpdate {
            average: Decibels(48.0),
            maximum: Decibels(62.0),
            timestamp: at,
        },
    )));
    store.snapshot(MonotonicTimestamp::from_millis(1_000))
}

#[test]
#[ignore = "needs the robins `edge` binary: set ROHI_ROBINS_EDGE"]
fn a_rohi_frame_is_accepted_by_the_robins_ingress() {
    let edge = std::env::var("ROHI_ROBINS_EDGE")
        .expect("set ROHI_ROBINS_EDGE to the path of the robins `edge` binary");
    let gateway = start_gateway(&edge);

    let ip = StdIp;
    let uptime = Uptime(Instant::now());
    // The snapshot is "generated" at 1 s of uptime: make the clock agree.
    std::thread::sleep(Duration::from_millis(1_050));
    let shared = ConnectivityShared::<NoopRawMutex>::new();
    let signer = MockSigner::default();
    let utc = SystemUtc;
    let transport = HttpTransport::new(
        NetworkHandle::new(&ip),
        Endpoint {
            host: "127.0.0.1",
            port: gateway.port,
        },
        StdDelay,
    );
    let mut service: ConnectivityService<'_, NoopRawMutex, _, _, _, _, _, _, _, 2> =
        ConnectivityService::new(
            ServiceConfig::new(7, 60_000),
            &shared,
            Fixed(snapshot()),
            &rohi_location::NoLocation,
            &uptime,
            &utc,
            &signer,
            OsNonce,
            transport,
            StdDelay,
        );

    block_on(service.produce());
    assert_eq!(service.queued(), 1, "{:?}", service.diagnostics());
    block_on(service.deliver());

    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.frames_delivered, 1, "{:?}", service.state());
    assert_eq!(service.queued(), 0);
}
