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

//! End-to-end tests: a real simulator process driven through the real `rohi`
//! client over the TCP control transport and the management interface.
//!
//! Every test runs its own simulator on free ports with its own storage, and
//! waits for the READY line and for observable state, never for a fixed time.

mod common;

use std::time::Duration;

use common::*;
use rohi::model::{ServiceHealth, ServiceState, Value};
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(20);

/// A scenario with fast sensors and a short freshness window.
fn scenario(dir: &TempDir, extra: &str) -> std::path::PathBuf {
    let path = dir.path().join("scenario.toml");
    std::fs::write(
        &path,
        format!(
            "[sensors]\nco2 = 700\ninterval = \"100ms\"\n\n[freshness]\nstale_after = \"1s\"\n\n{extra}"
        ),
    )
    .unwrap();
    path
}

#[tokio::test(flavor = "current_thread")]
async fn starts_with_empty_storage_and_answers_get_info() {
    let sim = Sim::start(&Launch::new());
    let mut device = sim.device().await;

    let info = device.info().await.unwrap();
    assert!(info.firmware.starts_with("altruist-urban-"), "{info:?}");
    assert_eq!(info.board, "urban-std");

    let services = device.services().await.unwrap();
    let names: Vec<_> = services.iter().map(|s| s.name.as_str()).collect();
    for expected in ["wifi", "network", "sensors", "identity", "time"] {
        assert!(names.contains(&expected), "{names:?}");
    }
    drop(device);
    assert!(sim.stop().success());
}

#[tokio::test(flavor = "current_thread")]
async fn sensor_values_flow_through_the_measurement_runtime_and_age() {
    let dir = TempDir::new().unwrap();
    let sim = Sim::start(&Launch::new().scenario(&scenario(&dir, "")));
    let mut device = sim.device().await;

    let co2 = eventually("the scenario CO2", WAIT, async || {
        f32_of(fields(&mut device, "sensors").await.get("scd4x.co2")).filter(|v| *v == 700.0)
    })
    .await;
    assert_eq!(co2, 700.0);
    let all = fields(&mut device, "sensors").await;
    assert_eq!(f32_of(all.get("bmx280.temperature")), Some(22.5));
    assert_eq!(f32_of(all.get("bmx280.pressure")), Some(101_300.0));

    sim.manage_ok("set co2 1200");
    eventually("the new CO2", WAIT, async || {
        f32_of(fields(&mut device, "sensors").await.get("scd4x.co2")).filter(|v| *v == 1200.0)
    })
    .await;

    eventually("a valid measurement", WAIT, async || {
        sim.manage_ok("measurements")
            .contains("measurement bmx280.temperature state=Valid")
            .then_some(())
    })
    .await;
    sim.manage_ok("fail bmx280 nodata");
    eventually("the measurement to go stale", WAIT, async || {
        sim.manage_ok("measurements")
            .contains("measurement bmx280.temperature state=Stale")
            .then_some(())
    })
    .await;
    sim.manage_ok("recover bmx280");
    eventually("the measurement to be fresh again", WAIT, async || {
        sim.manage_ok("measurements")
            .contains("measurement bmx280.temperature state=Valid")
            .then_some(())
    })
    .await;
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn a_failing_sensor_is_reported_and_recovers() {
    let dir = TempDir::new().unwrap();
    let sim = Sim::start(&Launch::new().scenario(&scenario(&dir, "")));
    let mut device = sim.device().await;

    eventually("a healthy sensors service", WAIT, async || {
        let (state, health, ready, issues) = summary(&mut device, "sensors").await;
        (state == ServiceState::Running
            && health == ServiceHealth::Ok
            && ready
            && issues.is_empty())
        .then_some(())
    })
    .await;

    sim.manage_ok("fail scd4x read");
    let issues = eventually("the sensor fault", WAIT, async || {
        let (_, health, _, issues) = summary(&mut device, "sensors").await;
        (health == ServiceHealth::Degraded && issues.contains(&"scd4x_fault".to_owned()))
            .then_some(issues)
    })
    .await;
    assert_eq!(issues, ["scd4x_fault"]);

    sim.manage_ok("recover scd4x");
    eventually("the sensor to recover", WAIT, async || {
        let (_, health, ready, issues) = summary(&mut device, "sensors").await;
        (health == ServiceHealth::Ok && ready && issues.is_empty()).then_some(())
    })
    .await;
    eventually("valid CO2 again", WAIT, async || {
        f32_of(fields(&mut device, "sensors").await.get("scd4x.co2")).filter(|v| *v == 700.0)
    })
    .await;
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn configuration_survives_a_restart() {
    let dir = TempDir::new().unwrap();
    let storage = dir.path().join("device.flash");

    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    assert_eq!(config_text(&mut device, "wifi", wifi::SSID).await, None);
    device
        .config_patch(
            "wifi",
            &[
                set(wifi::SSID, text("persisted-net")),
                set(wifi::PASSWORD, text("correct horse")),
            ],
            false,
        )
        .await
        .unwrap();
    drop(device);
    assert!(sim.stop().success());

    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    assert_eq!(
        config_text(&mut device, "wifi", wifi::SSID)
            .await
            .as_deref(),
        Some("persisted-net")
    );
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn an_interrupted_config_write_recovers_the_committed_generation() {
    let dir = TempDir::new().unwrap();
    let storage = dir.path().join("device.flash");

    let mut sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    device
        .config_patch("wifi", &[set(wifi::SSID, text("generation-one"))], false)
        .await
        .unwrap();

    // Power is lost while the next generation is being programmed.
    sim.manage_ok("storage power-loss program 8");
    let outcome = device
        .config_patch("wifi", &[set(wifi::SSID, text("generation-two"))], false)
        .await;
    assert!(outcome.is_err(), "the device lost power: {outcome:?}");
    drop(device);
    let status = sim
        .wait_exit(Duration::from_secs(10))
        .expect("the device stopped");
    assert_eq!(status.code(), Some(70), "{}", sim.log());

    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    assert_eq!(
        config_text(&mut device, "wifi", wifi::SSID)
            .await
            .as_deref(),
        Some("generation-one")
    );
    // The recovered store is fully usable.
    device
        .config_patch("wifi", &[set(wifi::SSID, text("generation-three"))], false)
        .await
        .unwrap();
    assert_eq!(
        config_text(&mut device, "wifi", wifi::SSID)
            .await
            .as_deref(),
        Some("generation-three")
    );
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn wifi_connects_backs_off_and_recovers() {
    let sim = Sim::start(&Launch::new());
    let mut device = sim.device().await;

    // Not configured: the manager is idle.
    let (state, _, ready, _) = summary(&mut device, "wifi").await;
    assert_eq!((state, ready), (ServiceState::Disabled, false));

    device
        .config_patch(
            "wifi",
            &[
                set(wifi::SSID, text("ROHI-LAB")),
                set(wifi::PASSWORD, text("correct horse")),
                set(wifi::BACKOFF_INITIAL, Value::U64(100)),
                set(wifi::BACKOFF_MAX, Value::U64(400)),
            ],
            false,
        )
        .await
        .unwrap();
    eventually("the Wi-Fi link", WAIT, async || {
        let (state, health, ready, _) = summary(&mut device, "wifi").await;
        (state == ServiceState::Running && health == ServiceHealth::Ok && ready).then_some(())
    })
    .await;
    eventually("the network to be ready", WAIT, async || {
        let (state, _, ready, _) = summary(&mut device, "network").await;
        (state == ServiceState::Running && ready).then_some(())
    })
    .await;

    // The access point disappears: the manager reports it and keeps retrying.
    sim.manage_ok("ap ROHI-LAB down");
    let issues = eventually("the Wi-Fi failure", WAIT, async || {
        let (_, _, ready, issues) = summary(&mut device, "wifi").await;
        (!ready && issues.contains(&"network_not_found".to_owned())).then_some(issues)
    })
    .await;
    assert!(!issues.is_empty());

    sim.manage_ok("ap ROHI-LAB up");
    eventually("the Wi-Fi to recover", WAIT, async || {
        let (state, health, ready, issues) = summary(&mut device, "wifi").await;
        (state == ServiceState::Running
            && health == ServiceHealth::Ok
            && ready
            && issues.is_empty())
        .then_some(())
    })
    .await;

    // Wrong credentials flow through the same error model.
    sim.manage_ok("wifi fail authentication");
    sim.manage_ok("wifi drop");
    let issues = eventually("an authentication failure", WAIT, async || {
        let (_, _, ready, issues) = summary(&mut device, "wifi").await;
        (!ready && issues.contains(&"authentication_failed".to_owned())).then_some(issues)
    })
    .await;
    assert!(!issues.is_empty());
    sim.manage_ok("wifi recover");
    eventually("recovery after the fault", WAIT, async || {
        let (_, _, ready, _) = summary(&mut device, "wifi").await;
        ready.then_some(())
    })
    .await;
    drop(device);
    sim.stop();
}

/// A link that delivers every request and never reads its answer, like a
/// client that goes away or gives up: the cancelled call of the device side.
struct Abandoning {
    link: rohi::TcpLink,
}

impl rohi::ClientTransport for Abandoning {
    fn max_payload(&self) -> usize {
        self.link.max_payload()
    }

    fn framing(&self) -> rohi::Framing {
        self.link.framing()
    }

    async fn send(&mut self, chunk: &[u8]) -> rohi::Result<()> {
        self.link.send(chunk).await
    }

    async fn recv(&mut self) -> rohi::Result<Vec<u8>> {
        Err(rohi::Error::Timeout)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn an_abandoned_request_leaves_the_services_usable() {
    let sim = Sim::start(&Launch::new());

    // The client sends a configuration change and disappears before the answer.
    let link = rohi::TcpLink::connect(&sim.control.to_string(), Duration::from_secs(10))
        .await
        .unwrap();
    let mut abandoning = rohi::Device::new(Abandoning { link });
    let abandoned = abandoning
        .config_patch("wifi", &[set(wifi::SSID, text("abandoned"))], false)
        .await;
    assert!(abandoned.is_err());
    drop(abandoning);

    // The device serves the next client; the abandoned change was either
    // committed whole or not at all, and every service answers.
    let mut device = sim.device().await;
    let ssid = config_text(&mut device, "wifi", wifi::SSID).await;
    assert!(
        matches!(ssid.as_deref(), None | Some("abandoned")),
        "{ssid:?}"
    );
    let status = device.services_status().await.unwrap();
    assert!(!status.services.is_empty());
    device
        .config_patch("wifi", &[set(wifi::SSID, text("after"))], false)
        .await
        .unwrap();
    assert_eq!(
        config_text(&mut device, "wifi", wifi::SSID)
            .await
            .as_deref(),
        Some("after")
    );
    drop(device);
    sim.stop();
}

#[test]
fn a_busy_control_port_is_a_startup_error_not_a_panic() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_altruist-urban"))
        .args(["--ephemeral", "--control-port", &port.to_string()])
        .args(["--manage-port", "0"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(75));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot listen"));
}

#[test]
fn an_invalid_scenario_is_a_startup_error_not_a_panic() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("broken.toml");
    std::fs::write(&path, "[[events]]\nat = \"soon\"\naction = \"explode\"\n").unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_altruist-urban"))
        .args(["--ephemeral", "--control-port", "0", "--scenario"])
        .arg(&path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(65));
}

#[test]
fn bad_arguments_are_a_usage_error() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_altruist-urban"))
        .arg("--no-such-flag")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

/// Reads one response record from a raw connection.
fn read_record(stream: &mut std::net::TcpStream) -> Option<rohi_control_proto::control::Response> {
    use std::io::Read as _;

    let mut header = [0u8; 4];
    stream.read_exact(&mut header).ok()?;
    let len = rohi_control_tcp::frame::decode_header(header, 2048).ok()?;
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).ok()?;
    rohi_control_proto::decode(&payload).ok()
}

fn is_invalid_argument(response: &rohi_control_proto::control::Response) -> bool {
    use rohi_control_proto::control::{ErrorCode, Response_::Response as Reply};

    matches!(&response.response, Some(Reply::Error(error)) if error.code == ErrorCode::InvalidArgument)
}

#[test]
fn the_tcp_framing_rejects_oversized_and_malformed_records() {
    use std::io::{Read as _, Write as _};

    let sim = Sim::start(&Launch::new());

    // A header above the request limit is answered once and the connection ends.
    let mut stream = std::net::TcpStream::connect(sim.control).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .write_all(&rohi_control_tcp::frame::encode_header(1 << 20))
        .unwrap();
    assert!(is_invalid_argument(&read_record(&mut stream).unwrap()));
    let mut rest = Vec::new();
    assert_eq!(stream.read_to_end(&mut rest).unwrap_or(0), 0);

    // An empty record is rejected the same way.
    let mut stream = std::net::TcpStream::connect(sim.control).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(&0u32.to_le_bytes()).unwrap();
    assert!(is_invalid_argument(&read_record(&mut stream).unwrap()));

    // A record that is not a request is answered and the connection stays usable.
    let mut stream = std::net::TcpStream::connect(sim.control).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let junk = [0xffu8; 9];
    stream
        .write_all(&rohi_control_tcp::frame::encode_header(junk.len() as u32))
        .unwrap();
    stream.write_all(&junk).unwrap();
    assert!(is_invalid_argument(&read_record(&mut stream).unwrap()));
    drop(stream);

    // The device keeps serving real clients after all of it.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut device = sim.device().await;
        assert!(device.info().await.is_ok());
    });
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn a_client_can_disconnect_and_reconnect() {
    let sim = Sim::start(&Launch::new());
    for _ in 0..3 {
        let mut device = sim.device().await;
        assert!(device.info().await.is_ok());
        drop(device);
    }
    sim.stop();
}

#[test]
fn a_remote_bind_needs_an_explicit_unsafe_flag() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_altruist-urban"))
        .args(["--ephemeral", "--control-port", "0", "--bind", "0.0.0.0"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(75));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--unsafe-allow-remote"));
}

#[test]
fn the_ready_line_names_the_interfaces_that_run() {
    let sim = Sim::start(&Launch::new());
    assert!(sim.control.ip().is_loopback());
    assert!(sim.manage.ip().is_loopback());
    sim.stop();
}
