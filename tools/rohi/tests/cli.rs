//! The `rohi` command line without Bluetooth: parsing, help, output contract,
//! device selection helpers, status, Wi-Fi workflows and backup/restore,
//! run through `rohi::cli::execute` against the in-memory device.

mod common;

use std::cell::RefCell;
use std::io::{self, Write};
use std::rc::Rc;
use std::time::Duration;

use clap::{CommandFactory, Parser};
use common::*;
use rohi::cli::{
    Cli, CliError, Command, Ctx, IdentityAction, Io, PartialRestore, execute, prepare,
};
use rohi::{Device, Error};
use rohi_control::{Capabilities, Controller, OpenPolicy};
use rohi_control_ble::ControlBle;
use rohi_net::{
    WifiConfig as NetConfig, WifiError as NetError, WifiPassword, WifiSsid, WifiState as NetState,
    WifiStatus as NetStatus,
};
use serde_json::{Value, json};

const DEV_PHRASE: &str = "bottom drive obey lake curtain smoke basket hold race lonely fit walk";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct Buf(Rc<RefCell<Vec<u8>>>);

impl Buf {
    fn text(&self) -> String {
        String::from_utf8(self.0.borrow().clone()).unwrap()
    }
}

impl Write for Buf {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// How the terminal behaves in a test.
#[derive(Default)]
struct Terminal<'a> {
    interactive: bool,
    input: &'a [&'a str],
    password_env: Option<&'a str>,
}

struct Outcome {
    services: Services,
    result: Result<(), CliError>,
    stdout: String,
    stderr: String,
}

impl Outcome {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|error| panic!("stdout is not JSON ({error}): {:?}", self.stdout))
    }

    fn error(&self) -> &CliError {
        self.result.as_ref().expect_err("the command succeeded")
    }

    fn device_error(&self) -> &Error {
        match self.error() {
            CliError::Device(error) => error,
            other => panic!("not a device error: {other:?}"),
        }
    }

    fn requests(&self) -> Vec<&'static str> {
        self.services.requests.borrow().clone()
    }

    /// Requests that changed something on the device.
    fn mutations(&self) -> Vec<&'static str> {
        self.requests()
            .into_iter()
            .filter(|request| {
                matches!(
                    *request,
                    "set" | "set_connectivity" | "store_identity" | "erase_identity"
                )
            })
            .collect()
    }

    fn password(&self) -> Option<Vec<u8>> {
        self.services
            .config
            .borrow()
            .as_ref()
            .and_then(|config| config.password.as_ref().map(|p| p.expose_secret().to_vec()))
    }

    fn ssid(&self) -> Option<Vec<u8>> {
        self.services
            .config
            .borrow()
            .as_ref()
            .map(|config| config.ssid.as_bytes().to_vec())
    }
}

fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(std::iter::once("rohi").chain(args.iter().copied()))
}

async fn run_with(services: Services, args: &[&str], terminal: Terminal<'_>) -> Outcome {
    let cli = parse(args).unwrap();
    let (stdout, stderr) = (Buf::default(), Buf::default());
    let io = Io::scripted(
        stdout.clone(),
        stderr.clone(),
        terminal.interactive,
        terminal.input,
    );
    let mut ctx = Ctx::new(io, cli.output);
    ctx.uri = Some("ble://BENCH-1234".into());
    ctx.wifi_password = terminal.password_env.map(str::to_owned);
    ctx.poll_interval = Duration::from_millis(1);

    let prepared = match prepare(&cli.command) {
        Ok(prepared) => prepared,
        Err(error) => {
            return Outcome {
                services,
                result: Err(error),
                stdout: stdout.text(),
                stderr: stderr.text(),
            };
        }
    };
    let (client, device) = link(244);
    let controller = Controller::new(services, OpenPolicy);
    let mut server = ControlBle::new(device, &controller);
    let command = cli.command;
    let result = tokio::select! {
        () = async { server.run().await } => unreachable!(),
        result = async {
            let mut device = Device::new(client);
            execute(&mut device, command, prepared, &mut ctx).await
        } => result,
    };
    drop(server);
    Outcome {
        services: controller_into_services(controller),
        result,
        stdout: stdout.text(),
        stderr: stderr.text(),
    }
}

async fn run(services: Services, args: &[&str]) -> Outcome {
    run_with(services, args, Terminal::default()).await
}

fn services_with_wifi(ssid: &str, password: Option<&str>) -> Services {
    let services = Services::new();
    *services.config.borrow_mut() = Some(NetConfig::new(
        WifiSsid::from_bytes(ssid.as_bytes()).unwrap(),
        password.map(|p| WifiPassword::from_bytes(p.as_bytes()).unwrap()),
    ));
    services
}

fn script_wifi(services: &Services, steps: Vec<(NetStatus, Option<&str>)>) {
    let mut wifi = services.wifi.borrow_mut();
    wifi.script = steps
        .into_iter()
        .map(|(status, ssid)| (status, ssid.map(network)))
        .collect();
}

struct TempFile(std::path::PathBuf);

impl TempFile {
    fn new(name: &str, contents: Option<&str>) -> Self {
        let path = std::env::temp_dir().join(format!("rohi-cli-{}-{name}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        if let Some(contents) = contents {
            std::fs::write(&path, contents).unwrap();
        }
        Self(path)
    }

    fn path(&self) -> &str {
        self.0.to_str().unwrap()
    }

    fn read(&self) -> String {
        std::fs::read_to_string(&self.0).unwrap()
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

// ---------------------------------------------------------------------------
// Parser and help
// ---------------------------------------------------------------------------

fn help(args: &[&str]) -> String {
    let argv = std::iter::once("rohi")
        .chain(args.iter().copied())
        .chain(["--help"]);
    Cli::try_parse_from(argv).err().unwrap().to_string()
}

fn golden(name: &str, actual: &str) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing {name}; run with UPDATE_GOLDEN=1 to create it"));
    assert_eq!(
        actual, expected,
        "{name} changed; review the diff and run with UPDATE_GOLDEN=1 to accept it"
    );
}

#[test]
fn help_texts_are_snapshots() {
    let pages: &[&[&str]] = &[
        &[],
        &["scan"],
        &["status"],
        &["sensors"],
        &["config"],
        &["wifi"],
        &["wifi", "connect"],
        &["wifi", "configure"],
        &["wifi", "forget"],
        &["connectivity"],
        &["connectivity", "set"],
        &["time"],
        &["backup"],
        &["backup", "export"],
        &["backup", "import"],
        &["identity"],
        &["identity", "provision"],
        &["identity", "erase"],
    ];
    for page in pages {
        let name = format!("help-{}.txt", page.join("-")).replace("help-.txt", "help.txt");
        golden(&name, &help(page));
    }
}

#[test]
fn help_groups_options_and_hides_transport_tuning() {
    let top = help(&[]);
    for heading in ["Target:", "Output:", "Connection:"] {
        assert!(top.contains(heading), "{heading}\n{top}");
    }
    assert!(top.contains("--device <DEVICE>"));
    assert!(top.contains("ROHI_DEVICE"));
    assert!(top.contains("--output <FORMAT>"));
    assert!(top.contains("--scan-timeout <SECONDS>"));
    assert!(!top.contains("--payload"));
    assert!(!top.contains("--json"));
    assert!(!top.contains("Examples:"));
}

fn examples(command: &clap::Command, found: &mut Vec<String>) {
    let text = command
        .get_after_help()
        .map(ToString::to_string)
        .unwrap_or_default();
    for line in text.lines().map(str::trim) {
        if line.starts_with("rohi ") {
            found.push(line.split(" > ").next().unwrap().to_owned());
        }
    }
    for subcommand in command.get_subcommands() {
        examples(subcommand, found);
    }
}

#[test]
fn every_example_in_the_help_parses() {
    let mut found = Vec::new();
    examples(&Cli::command(), &mut found);
    assert!(found.len() > 30, "{found:#?}");
    for example in found {
        let argv: Vec<_> = example.split_whitespace().collect();
        if let Err(error) = Cli::try_parse_from(argv) {
            panic!("`{example}` does not parse: {error}");
        }
    }
}

#[test]
fn info_and_the_json_flag_are_gone() {
    let error = parse(&["info"]).err().unwrap();
    assert_eq!(error.kind(), clap::error::ErrorKind::InvalidSubcommand);
    let cli = Cli::command();
    assert!(
        cli.get_subcommands()
            .all(|c| c.get_name() != "info" && c.get_all_aliases().all(|a| a != "info"))
    );

    let error = parse(&["--json", "status"]).err().unwrap();
    assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    assert!(parse(&["-o", "json", "status"]).is_ok());
    assert!(parse(&["--output", "human", "status"]).is_ok());
    assert!(parse(&["-o", "yaml", "status"]).is_err());
}

#[test]
fn usage_errors_have_exit_code_2() {
    for args in [&["bogus"][..], &["wifi", "connect"], &["backup", "import"]] {
        assert_eq!(parse(args).err().unwrap().exit_code(), 2, "{args:?}");
    }
}

#[test]
fn wifi_connect_arguments() {
    assert!(parse(&["wifi", "connect", "office", "--open", "--password-stdin"]).is_err());
    assert!(parse(&["wifi", "connect", "office", "--no-wait", "--wait", "5"]).is_err());
    assert!(parse(&["wifi", "connect", "office", "--wait", "5"]).is_ok());
    assert!(parse(&["backup", "import", "f.json", "--dry-run", "--yes"]).is_err());
}

#[test]
fn device_selector_comes_from_the_flag() {
    let cli = parse(&["-d", "A12F", "status"]).unwrap();
    assert_eq!(cli.device.as_deref(), Some("A12F"));
    let cli = parse(&["--device", "ble://ALTRUIST-URBAN-A12F", "status"]).unwrap();
    assert_eq!(cli.device.as_deref(), Some("ble://ALTRUIST-URBAN-A12F"));
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn status_is_one_snapshot_of_the_device() {
    let outcome = run(Services::new(), &["status"]).await;
    outcome.result.as_ref().unwrap();
    assert_eq!(
        outcome.stdout,
        "\
Device
  Firmware    fake-1.0.0
  Board       bench-esp32c3
  URI         ble://BENCH-1234

Wi-Fi
  State       connected
  Network     office
  Signal      -55 dBm
  Address     10.0.0.7
  Gateway     10.0.0.1

Time
  State       unsynchronized

Identity
  State       not provisioned
"
    );
    assert!(outcome.stderr.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn status_json_is_the_same_snapshot() {
    let outcome = run(Services::new(), &["-o", "json", "status"]).await;
    outcome.result.as_ref().unwrap();
    assert_eq!(
        outcome.json(),
        json!({
            "schema_version": 1,
            "connection": {"uri": "ble://BENCH-1234"},
            "device": {"firmware": "fake-1.0.0", "board": "bench-esp32c3"},
            "capabilities": [
                "sensors", "config", "wifi_scan", "wifi_status", "wifi_control", "identity",
                "time", "connectivity"
            ],
            "wifi": {
                "state": "connected",
                "failure": null,
                "ssid": "office",
                "signal_dbm": -55,
                "address": "10.0.0.7",
                "gateway": "10.0.0.1",
                "dns": null
            },
            "time": {
                "state": "unsynchronized",
                "unix_time_ms": null,
                "source": null,
                "sync_age_ms": null,
                "uncertainty_ms": null,
                "consecutive_failures": 0,
                "last_failure": null
            },
            "identity": {"state": "not provisioned", "address": null, "public_key": null}
        })
    );
    assert!(outcome.stderr.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn time_shows_the_wall_clock_only_when_the_device_has_one() {
    use core::time::Duration;
    use rohi_control::DeviceTime;
    use rohi_time::{SyncFailure, TimeSourceKind, TimeStatus, TimeSyncState, UnixTime};

    let outcome = run(Services::new(), &["time"]).await;
    outcome.result.as_ref().unwrap();
    assert_eq!(outcome.stdout, "Time\n  State       unsynchronized\n");

    let mut services = Services::new();
    services.time = DeviceTime {
        unix_time: Some(UnixTime::from_millis(1_780_000_000_250)),
        status: TimeStatus {
            state: TimeSyncState::Stale,
            source: Some(TimeSourceKind::Ntp),
            last_sync_age: Some(Duration::from_secs(7_200)),
            uncertainty: Some(Duration::from_millis(40)),
            consecutive_failures: 2,
            last_failure: Some(SyncFailure::Timeout),
        },
    };
    let outcome = run(services, &["time"]).await;
    outcome.result.as_ref().unwrap();
    assert_eq!(
        outcome.stdout,
        format!(
            "\
Time
  State       stale
  Date Time   {}
  Source      ntp
  Last sync   7200 s ago
  Accuracy    +-40 ms
  Failures    2 (last: timeout)
",
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(1_780_000_000_250)
                .unwrap()
                .with_timezone(&chrono::Local)
                .to_rfc3339()
        )
    );

    let json = run(outcome.services, &["-o", "json", "time"]).await.json();
    assert_eq!(json["unix_time_ms"], 1_780_000_000_250_u64);
    assert_eq!(json["source"], "ntp");
    assert_eq!(json["last_failure"], "timeout");
}

#[tokio::test(flavor = "current_thread")]
async fn a_provisioned_identity_shows_its_robonomics_address() {
    use sp_core::crypto::{AccountId32, Ss58Codec};

    let seed = TempFile::new("status.suri", Some(DEV_PHRASE));
    let provision = run(
        Services::new(),
        &["id", "provision", "-n", "42", "--seed", seed.path()],
    )
    .await;
    provision.result.as_ref().unwrap();
    let key = rohi::derive_identity(DEV_PHRASE, 42).unwrap().public_key();

    let status = run(provision.services, &["-o", "json", "status"]).await;
    status.result.as_ref().unwrap();
    let json = status.json();
    let address = json["identity"]["address"].as_str().unwrap();
    let (decoded, format) = AccountId32::from_ss58check_with_version(address).unwrap();
    assert_eq!(<[u8; 32]>::from(decoded), key.to_bytes());
    assert_eq!(u16::from(format), 32);
    assert_eq!(json["identity"]["public_key"], key.to_string());
    assert_eq!(json["identity"]["state"], "provisioned");

    let status = run(status.services, &["status"]).await;
    assert!(
        status.stdout.contains(&format!("Address     {address}")),
        "{}",
        status.stdout
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_device_without_wifi_still_has_a_status() {
    let mut services = Services::new();
    services.capabilities = Capabilities::SENSORS
        .union(Capabilities::CONFIG)
        .union(Capabilities::IDENTITY);
    let outcome = run(services, &["-o", "json", "status"]).await;
    outcome.result.as_ref().unwrap();
    let json = outcome.json();
    assert!(json.get("wifi").is_none(), "{json}");
    assert_eq!(
        json["capabilities"],
        json!(["sensors", "config", "identity"])
    );

    let mut services = Services::new();
    services.capabilities = Capabilities::NONE;
    let outcome = run(services, &["status"]).await;
    outcome.result.as_ref().unwrap();
    assert!(!outcome.stdout.contains("Wi-Fi"));
    assert!(!outcome.stdout.contains("Identity"));
    assert!(outcome.stdout.contains("Firmware    fake-1.0.0"));
}

/// A device that never answers.
struct Silent;

impl rohi::ClientTransport for Silent {
    fn max_payload(&self) -> usize {
        20
    }

    async fn send(&mut self, _: &[u8]) -> rohi::Result<()> {
        Ok(())
    }

    async fn recv(&mut self) -> rohi::Result<Vec<u8>> {
        std::future::pending().await
    }
}

#[tokio::test(flavor = "current_thread")]
async fn transport_failures_still_fail_the_status() {
    let mut device = Device::new(Silent);
    device.set_timeout(Duration::from_millis(20));
    let mut ctx = Ctx::new(
        Io::scripted(Buf::default(), Buf::default(), false, &[]),
        rohi::cli::OutputFormat::Human,
    );
    let command = parse(&["status"]).unwrap().command;
    let error = execute(&mut device, command, Default::default(), &mut ctx)
        .await
        .unwrap_err();
    assert!(matches!(error, CliError::Device(Error::Timeout)));
    let rendered = error.diagnostic().to_string();
    assert!(rendered.starts_with("error: timed out waiting for the device\n"));
    assert!(rendered.contains("hint: "));
}

// ---------------------------------------------------------------------------
// Results and their formats
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn results_have_cli_owned_json_schemas() {
    let outcome = run(Services::new(), &["-o", "json", "sensors"]).await;
    outcome.result.as_ref().unwrap();
    let json = outcome.json();
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["uptime_ms"], 5_000);
    let first = &json["sensors"][0]["measurements"][0];
    assert!(first["type"].is_string() && first["unit"].is_string());

    let outcome = run(
        services_with_wifi("office", Some("pw")),
        &["-o", "json", "config"],
    )
    .await;
    outcome.result.as_ref().unwrap();
    assert_eq!(
        outcome.json(),
        json!({
            "schema_version": 1,
            "wifi": {"ssid": "office", "password_configured": true},
            "connectivity": null
        })
    );
    assert!(!outcome.stdout.contains("\"pw\""));

    let outcome = run(Services::new(), &["-o", "json", "wifi", "scan"]).await;
    outcome.result.as_ref().unwrap();
    let json = outcome.json();
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["networks"].as_array().unwrap().len(), 16);
    assert_eq!(json["networks"][0]["signal_dbm"], -40);
    assert_eq!(json["networks"][0]["security"], "wpa2");

    let outcome = run(Services::new(), &["-o", "json", "wifi", "status"]).await;
    outcome.result.as_ref().unwrap();
    assert_eq!(outcome.json()["state"], "connected");
    assert_eq!(outcome.json()["address"], "10.0.0.7");

    let outcome = run(Services::new(), &["-o", "json", "wifi", "reconnect"]).await;
    outcome.result.as_ref().unwrap();
    assert_eq!(outcome.json(), json!({"schema_version": 1, "ok": true}));
}

#[tokio::test(flavor = "current_thread")]
async fn human_results_go_to_stdout_only() {
    for args in [
        &["sensors"][..],
        &["config"],
        &["wifi"],
        &["wifi", "scan"],
        &["id"],
    ] {
        let outcome = run(Services::new(), args).await;
        outcome.result.as_ref().unwrap();
        assert!(!outcome.stdout.is_empty(), "{args:?}");
        assert!(outcome.stderr.is_empty(), "{args:?}: {}", outcome.stderr);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_unsupported_capability_is_a_clear_diagnostic() {
    let mut services = Services::new();
    services.capabilities = Capabilities::SENSORS;
    let outcome = run(services, &["wifi", "scan"]).await;
    assert!(outcome.requests().is_empty());
    assert_eq!(
        outcome.error().diagnostic().to_string(),
        "error: this device does not support wifi scan"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_failing_stdout_fails_the_command_and_a_closed_pipe_is_quiet() {
    struct Failing(io::ErrorKind);

    impl Write for Failing {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(self.0.into())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(self.0.into())
        }
    }

    for (kind, quiet) in [
        (io::ErrorKind::BrokenPipe, true),
        (io::ErrorKind::PermissionDenied, false),
    ] {
        let (client, device) = link(244);
        let controller = Controller::new(Services::new(), OpenPolicy);
        let mut server = ControlBle::new(device, &controller);
        let error = tokio::select! {
                () = async { server.run().await } => unreachable!(),
                result = async {
                    let mut ctx = Ctx::new(
                        Io::scripted(Failing(kind), Buf::default(), false, &[]),
                        rohi::cli::OutputFormat::Human,
                    );
                    let mut device = Device::new(client);
                    let command = parse(&["sensors"]).unwrap().command;
                    execute(&mut device, command, Default::default(), &mut ctx).await
                } => result.unwrap_err(),
        };
        assert_eq!(error.is_broken_pipe(), quiet, "{kind:?}");
        assert!(matches!(error, CliError::Output(_)));
    }
}

// ---------------------------------------------------------------------------
// Confirmation, prompts and Wi-Fi
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn destructive_commands_ask_only_where_somebody_can_answer() {
    let forget = ["wifi", "forget"];

    // No terminal and no --yes: refuse, change nothing.
    let outcome = run(services_with_wifi("office", Some("pw")), &forget).await;
    assert!(matches!(outcome.error(), CliError::ConfirmationRequired));
    assert_eq!(
        outcome.error().diagnostic().to_string(),
        "error: confirmation required in non-interactive mode\n\nhint: pass --yes to confirm"
    );
    assert!(outcome.mutations().is_empty());

    // A terminal: the question is on stderr, the result on stdout.
    let terminal = Terminal {
        interactive: true,
        input: &["y"],
        ..Default::default()
    };
    let outcome = run_with(services_with_wifi("office", Some("pw")), &forget, terminal).await;
    outcome.result.as_ref().unwrap();
    assert_eq!(outcome.stdout, "Wi-Fi configuration removed\n");
    assert_eq!(
        outcome.stderr,
        "Remove the stored Wi-Fi configuration? [y/N] "
    );
    assert_eq!(outcome.mutations(), ["set"]);
    assert!(outcome.services.config.borrow().is_none());

    // Declining or giving up is a cancellation, not invalid input.
    for input in [&["n"][..], &[""], &[]] {
        let terminal = Terminal {
            interactive: true,
            input,
            ..Default::default()
        };
        let outcome = run_with(services_with_wifi("office", Some("pw")), &forget, terminal).await;
        assert!(matches!(outcome.error(), CliError::Cancelled), "{input:?}");
        assert!(outcome.mutations().is_empty());
    }

    // --yes never asks.
    let outcome = run(
        services_with_wifi("office", Some("pw")),
        &["wifi", "forget", "--yes"],
    )
    .await;
    outcome.result.as_ref().unwrap();
    assert!(outcome.stderr.is_empty());

    // Identity erase has its own warning.
    let terminal = Terminal {
        interactive: true,
        input: &["no"],
        ..Default::default()
    };
    let outcome = run_with(Services::new(), &["id", "erase"], terminal).await;
    assert!(matches!(outcome.error(), CliError::Cancelled));
    assert!(
        outcome
            .stderr
            .contains("lose its current cryptographic identity")
    );
    assert!(outcome.mutations().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn configure_prompts_for_the_password_on_a_terminal_only() {
    let terminal = Terminal {
        interactive: true,
        input: &["hunter22"],
        ..Default::default()
    };
    let outcome = run_with(
        Services::new(),
        &["wifi", "configure", "--ssid", "home"],
        terminal,
    )
    .await;
    outcome.result.as_ref().unwrap();
    assert_eq!(outcome.ssid().as_deref(), Some(&b"home"[..]));
    assert_eq!(outcome.password().as_deref(), Some(&b"hunter22"[..]));
    assert!(outcome.stderr.starts_with("Password for home"));
    assert!(!outcome.stdout.contains("hunter22") && !outcome.stderr.contains("hunter22"));

    // An empty answer keeps the stored password.
    let terminal = Terminal {
        interactive: true,
        input: &[""],
        ..Default::default()
    };
    let outcome = run_with(
        services_with_wifi("home", Some("stored-password")),
        &["wifi", "configure", "--ssid", "other"],
        terminal,
    )
    .await;
    outcome.result.as_ref().unwrap();
    assert_eq!(outcome.ssid().as_deref(), Some(&b"other"[..]));
    assert_eq!(outcome.password().as_deref(), Some(&b"stored-password"[..]));

    // Nobody to ask: say so instead of guessing.
    let outcome = run(Services::new(), &["wifi", "configure", "--ssid", "home"]).await;
    assert!(matches!(outcome.error(), CliError::PasswordRequired { .. }));
    assert!(outcome.mutations().is_empty());

    let outcome = run(
        Services::new(),
        &["wifi", "configure", "--ssid", "cafe", "--open"],
    )
    .await;
    outcome.result.as_ref().unwrap();
    assert_eq!(outcome.password(), None);
}

fn connecting() -> NetStatus {
    NetStatus::new(NetState::Connecting)
}

fn connected() -> NetStatus {
    NetStatus::new(NetState::Connected)
}

#[tokio::test(flavor = "current_thread")]
async fn wifi_connect_waits_for_the_connection() {
    let services = Services::new();
    // The device still describes the previous network at first.
    script_wifi(
        &services,
        vec![
            (connected(), Some("guests")),
            (connecting(), None),
            (connecting(), None),
            (connected(), Some("office")),
        ],
    );
    let terminal = Terminal {
        interactive: true,
        input: &["hunter22"],
        ..Default::default()
    };
    let outcome = run_with(services, &["wifi", "connect", "office"], terminal).await;
    outcome.result.as_ref().unwrap();
    assert_eq!(outcome.stdout, "Connected\nAddress: 10.0.0.7\n");
    assert!(outcome.stderr.contains("Connecting to office..."));
    assert_eq!(outcome.ssid().as_deref(), Some(&b"office"[..]));
    assert_eq!(outcome.password().as_deref(), Some(&b"hunter22"[..]));
}

#[tokio::test(flavor = "current_thread")]
async fn wifi_connect_json_says_stored_and_connected() {
    let services = Services::new();
    script_wifi(
        &services,
        vec![(connecting(), None), (connected(), Some("office"))],
    );
    let terminal = Terminal {
        password_env: Some("hunter22"),
        ..Default::default()
    };
    let outcome = run_with(
        services,
        &["-o", "json", "wifi", "connect", "office"],
        terminal,
    )
    .await;
    outcome.result.as_ref().unwrap();
    let json = outcome.json();
    assert_eq!(
        (&json["stored"], &json["connected"]),
        (&json!(true), &json!(true))
    );
    assert_eq!(json["wifi"]["address"], "10.0.0.7");
    assert_eq!(outcome.password().as_deref(), Some(&b"hunter22"[..]));
}

#[tokio::test(flavor = "current_thread")]
async fn wifi_connect_reports_failure_and_timeout_after_storing() {
    let services = Services::new();
    script_wifi(
        &services,
        vec![
            (connecting(), None),
            (
                NetStatus::failed(NetState::Error, NetError::AuthenticationFailed),
                None,
            ),
        ],
    );
    let terminal = Terminal {
        password_env: Some("wrong-password"),
        ..Default::default()
    };
    let outcome = run_with(services, &["wifi", "connect", "office"], terminal).await;
    let CliError::WifiNotConnected(reason) = outcome.error() else {
        panic!("{:?}", outcome.error());
    };
    assert_eq!(reason, "Wi-Fi failed: authentication failed");
    assert!(outcome.stdout.is_empty());
    // Stored is not connected: the configuration stays.
    assert_eq!(outcome.ssid().as_deref(), Some(&b"office"[..]));
    assert!(
        outcome
            .error()
            .diagnostic()
            .to_string()
            .contains("hint: the configuration is stored")
    );

    let services = Services::new();
    script_wifi(&services, vec![(connecting(), None)]);
    let terminal = Terminal {
        password_env: Some("password1"),
        ..Default::default()
    };
    let outcome = run_with(
        services,
        &["wifi", "connect", "office", "--wait", "0"],
        terminal,
    )
    .await;
    let CliError::WifiNotConnected(reason) = outcome.error() else {
        panic!("{:?}", outcome.error());
    };
    assert!(reason.contains("did not connect within 0 s"), "{reason}");
}

#[tokio::test(flavor = "current_thread")]
async fn wifi_connect_password_sources() {
    // --open removes the password and never asks.
    let outcome = run(
        services_with_wifi("home", Some("old")),
        &["wifi", "connect", "cafe", "--open", "--no-wait"],
    )
    .await;
    outcome.result.as_ref().unwrap();
    assert_eq!(outcome.password(), None);
    assert_eq!(outcome.ssid().as_deref(), Some(&b"cafe"[..]));
    assert_eq!(
        outcome.stdout,
        "Wi-Fi configuration stored. Follow `rohi wifi status` to see the connection.\n"
    );

    // --password-stdin reads the first line of stdin.
    let terminal = Terminal {
        input: &["from-stdin"],
        ..Default::default()
    };
    let outcome = run_with(
        Services::new(),
        &["wifi", "connect", "office", "--password-stdin", "--no-wait"],
        terminal,
    )
    .await;
    outcome.result.as_ref().unwrap();
    assert_eq!(outcome.password().as_deref(), Some(&b"from-stdin"[..]));

    // Empty stdin is an error, not an open network.
    let outcome = run(
        Services::new(),
        &["wifi", "connect", "office", "--password-stdin", "--no-wait"],
    )
    .await;
    assert!(matches!(outcome.error(), CliError::PasswordRequired { .. }));
    assert!(outcome.mutations().is_empty());

    // Neither a terminal, nor a source: no silent open network.
    let outcome = run(Services::new(), &["wifi", "connect", "office"]).await;
    let rendered = outcome.error().diagnostic().to_string();
    assert!(
        rendered.contains("the Wi-Fi password for office is required"),
        "{rendered}"
    );
    assert!(rendered.contains("--password-stdin"));
    assert!(outcome.mutations().is_empty());
}

// ---------------------------------------------------------------------------
// Backup
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn export_writes_the_backup_to_stdout_without_secrets() {
    let outcome = run(
        services_with_wifi("office", Some("top-secret")),
        &["backup", "export"],
    )
    .await;
    outcome.result.as_ref().unwrap();
    assert_eq!(
        outcome.json(),
        json!({
            "schema_version": 1,
            "source": {"firmware": "fake-1.0.0", "board": "bench-esp32c3"},
            "config": {
                "wifi": {"ssid": "office", "credential": "required"},
                "connectivity": null
            }
        })
    );
    assert!(!outcome.stdout.contains("top-secret"));
    assert!(outcome.stderr.is_empty());
    assert!(outcome.mutations().is_empty());

    // The backup format does not depend on the output format.
    let again = run(
        services_with_wifi("office", Some("top-secret")),
        &["-o", "json", "backup", "export"],
    )
    .await;
    assert_eq!(again.stdout, outcome.stdout);
}

#[tokio::test(flavor = "current_thread")]
async fn export_distinguishes_open_and_unconfigured() {
    let outcome = run(services_with_wifi("cafe", None), &["backup", "export"]).await;
    assert_eq!(
        outcome.json()["config"],
        json!({"wifi": {"ssid": "cafe", "credential": "none"}, "connectivity": null})
    );
    let outcome = run(Services::new(), &["backup", "export"]).await;
    assert_eq!(
        outcome.json()["config"],
        json!({"wifi": null, "connectivity": null})
    );
}

#[tokio::test(flavor = "current_thread")]
async fn export_to_a_file_reports_on_stdout() {
    let file = TempFile::new("export.json", None);
    let outcome = run(
        services_with_wifi("office", Some("pw")),
        &["backup", "export", file.path()],
    )
    .await;
    outcome.result.as_ref().unwrap();
    assert_eq!(
        outcome.stdout,
        format!("Backup written to {}\n", file.path())
    );
    let saved: Value = serde_json::from_str(&file.read()).unwrap();
    assert_eq!(saved["schema_version"], 1);

    let outcome = run(
        services_with_wifi("office", Some("pw")),
        &["-o", "json", "backup", "export", file.path()],
    )
    .await;
    assert_eq!(outcome.json()["file"], file.path());

    let outcome = run(
        Services::new(),
        &["backup", "export", "/nonexistent-directory/backup.json"],
    )
    .await;
    assert!(
        outcome
            .error()
            .diagnostic()
            .message
            .starts_with("cannot write /nonexistent-directory/backup.json")
    );
}

const OFFICE_BACKUP: &str = r#"{
  "schema_version": 1,
  "source": {"firmware": "altruist-urban-0.4.0", "board": "urban-esp32c3"},
  "config": {"wifi": {"ssid": "office", "credential": "required"}}
}"#;

#[tokio::test(flavor = "current_thread")]
async fn a_backup_restores_onto_another_device() {
    let source = run(
        services_with_wifi("office", Some("secret")),
        &["backup", "export"],
    )
    .await;
    let file = TempFile::new("move.json", Some(&source.stdout));

    let terminal = Terminal {
        interactive: true,
        input: &["new-secret", "y"],
        ..Default::default()
    };
    let target = run_with(
        Services::new(),
        &["backup", "import", file.path()],
        terminal,
    )
    .await;
    target.result.as_ref().unwrap();
    assert_eq!(target.ssid().as_deref(), Some(&b"office"[..]));
    assert_eq!(target.password().as_deref(), Some(&b"new-secret"[..]));
    assert!(
        target.stderr.contains("Wi-Fi SSID        <unset> → office"),
        "{}",
        target.stderr
    );
    assert!(target.stderr.contains("Wi-Fi password    required"));
    assert!(target.stderr.contains("Password for office: "));
    assert!(target.stderr.contains("Apply this configuration? [y/N] "));
    assert!(target.stdout.starts_with("Configuration restored."));
    assert!(!target.stdout.contains("new-secret") && !target.stderr.contains("new-secret"));
}

#[tokio::test(flavor = "current_thread")]
async fn dry_run_shows_the_changes_and_touches_nothing() {
    let file = TempFile::new("dry.json", Some(OFFICE_BACKUP));
    let outcome = run(
        services_with_wifi("guests", Some("pw")),
        &["backup", "import", file.path(), "--dry-run"],
    )
    .await;
    outcome.result.as_ref().unwrap();
    assert_eq!(
        outcome.stdout,
        "\
Backup:
  Source firmware   altruist-urban-0.4.0
  Source board      urban-esp32c3
  Target firmware   fake-1.0.0
  Target board      bench-esp32c3

Would apply:
  Wi-Fi SSID        guests → office
  Wi-Fi password    required

No changes were made.
"
    );
    assert!(outcome.stderr.is_empty());
    assert!(outcome.mutations().is_empty());
    assert_eq!(outcome.ssid().as_deref(), Some(&b"guests"[..]));

    let outcome = run(
        Services::new(),
        &["-o", "json", "backup", "import", file.path(), "--dry-run"],
    )
    .await;
    assert_eq!(
        outcome.json(),
        json!({
            "schema_version": 1,
            "ok": true,
            "dry_run": true,
            "changes": [{
                "section": "wifi",
                "ssid_from": null,
                "ssid_to": "office",
                "password": "required"
            }]
        })
    );
    assert!(outcome.mutations().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn import_needs_a_secret_or_fails_before_changing_anything() {
    let file = TempFile::new("secret.json", Some(OFFICE_BACKUP));
    let args = ["backup", "import", file.path(), "--yes"];

    // Nobody to ask, nothing supplied.
    let outcome = run(Services::new(), &args).await;
    let rendered = outcome.error().diagnostic().to_string();
    assert!(
        rendered.contains("error: the Wi-Fi password for office is required"),
        "{rendered}"
    );
    assert!(outcome.mutations().is_empty());

    // Supplied on stdin.
    let terminal = Terminal {
        input: &["piped-secret"],
        ..Default::default()
    };
    let args = [
        "backup",
        "import",
        file.path(),
        "--yes",
        "--wifi-password-stdin",
    ];
    let outcome = run_with(Services::new(), &args, terminal).await;
    outcome.result.as_ref().unwrap();
    assert_eq!(outcome.password().as_deref(), Some(&b"piped-secret"[..]));

    // Supplied through the environment.
    let terminal = Terminal {
        password_env: Some("env-secret"),
        ..Default::default()
    };
    let outcome = run_with(
        Services::new(),
        &["backup", "import", file.path(), "--yes"],
        terminal,
    )
    .await;
    outcome.result.as_ref().unwrap();
    assert_eq!(outcome.password().as_deref(), Some(&b"env-secret"[..]));

    // An empty interactive answer cannot stand for a password that is not there.
    let terminal = Terminal {
        interactive: true,
        input: &[""],
        ..Default::default()
    };
    let outcome = run_with(
        Services::new(),
        &["backup", "import", file.path()],
        terminal,
    )
    .await;
    assert!(matches!(outcome.error(), CliError::PasswordRequired { .. }));
    assert!(outcome.mutations().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn import_asks_for_confirmation_unless_told_not_to() {
    let file = TempFile::new("confirm.json", Some(OFFICE_BACKUP));
    let args = ["backup", "import", file.path(), "--wifi-password-stdin"];

    let terminal = Terminal {
        input: &["password1"],
        ..Default::default()
    };
    let outcome = run_with(Services::new(), &args, terminal).await;
    assert!(matches!(outcome.error(), CliError::ConfirmationRequired));
    assert!(outcome.mutations().is_empty());

    let terminal = Terminal {
        interactive: true,
        input: &["password1", "n"],
        ..Default::default()
    };
    let outcome = run_with(
        Services::new(),
        &["backup", "import", file.path()],
        terminal,
    )
    .await;
    assert!(matches!(outcome.error(), CliError::Cancelled));
    assert!(outcome.mutations().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn a_matching_device_needs_no_restore_and_no_secret() {
    let file = TempFile::new("same.json", Some(OFFICE_BACKUP));
    let outcome = run(
        services_with_wifi("office", Some("already")),
        &["backup", "import", file.path()],
    )
    .await;
    outcome.result.as_ref().unwrap();
    assert_eq!(
        outcome.stdout,
        "The device already matches the backup. No changes were made.\n"
    );
    assert!(outcome.mutations().is_empty());
    assert_eq!(outcome.password().as_deref(), Some(&b"already"[..]));
}

#[tokio::test(flavor = "current_thread")]
async fn an_explicitly_empty_section_forgets_the_network() {
    let file = TempFile::new(
        "forget.json",
        Some(
            r#"{"schema_version": 1, "source": {"firmware": "a", "board": "b"}, "config": {"wifi": null}}"#,
        ),
    );
    let outcome = run(
        services_with_wifi("office", Some("pw")),
        &["backup", "import", file.path(), "--yes"],
    )
    .await;
    outcome.result.as_ref().unwrap();
    assert!(outcome.services.config.borrow().is_none());

    // A backup without the section leaves the device alone.
    let file = TempFile::new(
        "untouched.json",
        Some(r#"{"schema_version": 1, "source": {"firmware": "a", "board": "b"}, "config": {}}"#),
    );
    let outcome = run(
        services_with_wifi("office", Some("pw")),
        &["backup", "import", file.path(), "--yes"],
    )
    .await;
    outcome.result.as_ref().unwrap();
    assert!(outcome.mutations().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn bad_backups_never_reach_the_device() {
    let cases = [
        (
            r#"{"schema_version": 3, "config": {}}"#,
            "error: backup schema version 3 is not supported by this version of rohi\n\n\
             hint: this version supports backup schema version 1",
        ),
        ("not json", "error: the file is not a valid backup"),
        (
            r#"{"schema_version": 1, "source": {"firmware": "a", "board": "b"}, "config": {"wifi": {"ssid": "x", "credential": "?"}}}"#,
            "error: the file is not a valid backup",
        ),
        (
            r#"{"schema_version": 1, "source": {"firmware": "a", "board": "b"}, "config": {"wifi": {"ssid": "", "credential": "none"}}}"#,
            "error: the backup is not valid: the Wi-Fi SSID is empty",
        ),
        (
            r#"{"schema_version": 1, "source": {"firmware": "a", "board": "b"}, "config": {"mqtt": {}, "location": {}}}"#,
            "error: the backup has sections this version of rohi cannot restore\n\n\
             unsupported:\n  location\n  mqtt\n\nhint: no changes were made",
        ),
    ];
    for (text, expected) in cases {
        let file = TempFile::new("bad.json", Some(text));
        let outcome = run(
            services_with_wifi("office", Some("pw")),
            &["backup", "import", file.path(), "--yes"],
        )
        .await;
        let rendered = outcome.error().diagnostic().to_string();
        assert!(rendered.starts_with(expected), "{text}\n{rendered}");
        assert!(outcome.requests().is_empty(), "{text}");
    }

    let outcome = run(
        Services::new(),
        &["backup", "import", "/nonexistent/backup.json"],
    )
    .await;
    assert!(
        outcome
            .error()
            .diagnostic()
            .message
            .starts_with("cannot read /nonexistent/backup.json")
    );
    assert!(outcome.requests().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn a_device_that_cannot_restore_a_section_is_left_alone() {
    let file = TempFile::new("incompatible.json", Some(OFFICE_BACKUP));
    let mut services = Services::new();
    services.capabilities = Capabilities::SENSORS;
    let outcome = run(
        services,
        &[
            "backup",
            "import",
            file.path(),
            "--yes",
            "--wifi-password-stdin",
        ],
    )
    .await;
    assert_eq!(
        outcome.error().diagnostic().to_string(),
        "error: this device cannot restore all sections in the backup\n\n\
         unsupported:\n  wifi\n\nhint: no changes were made"
    );
    assert!(outcome.mutations().is_empty());
}

#[test]
fn a_partial_restore_names_what_was_and_was_not_applied() {
    let error = CliError::PartialRestore(Box::new(PartialRestore {
        applied: vec!["wifi"],
        failed: "mqtt",
        pending: vec!["location"],
        source: Error::Device(rohi::model::ErrorCode::Busy),
    }));
    let rendered = error.diagnostic().to_string();
    assert!(rendered.contains("failed: mqtt"));
    assert!(rendered.contains("applied:\n  wifi"));
    assert!(rendered.contains("not applied:\n  location"));
    assert!(rendered.contains("not atomic"));
}

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

mod identity {
    use super::*;

    fn is_identity(command: &Command, check: impl Fn(&Option<IdentityAction>) -> bool) -> bool {
        matches!(command, Command::Identity { action } if check(action))
    }

    fn stored_key(outcome: &Outcome) -> Option<[u8; 32]> {
        outcome
            .services
            .identity
            .borrow()
            .as_ref()
            .map(|identity| identity.public_key().to_bytes())
    }

    #[test]
    fn id_is_an_alias_of_identity() {
        for name in ["identity", "id"] {
            let cli = parse(&[name]).unwrap();
            assert!(is_identity(&cli.command, Option::is_none), "{name}");

            let cli = parse(&[name, "provision", "-n", "42", "--seed", "master.suri"]).unwrap();
            assert!(
                is_identity(&cli.command, |action| matches!(
                    action,
                    Some(IdentityAction::Provision(p)) if p.number == 42
                )),
                "{name}"
            );

            let cli = parse(&[name, "erase", "--yes"]).unwrap();
            assert!(
                is_identity(&cli.command, |action| matches!(
                    action,
                    Some(IdentityAction::Erase { yes: true })
                )),
                "{name}"
            );
        }
    }

    #[test]
    fn number_has_a_long_and_a_short_form() {
        for flag in ["-n", "--number"] {
            let cli = parse(&["id", "provision", flag, "7", "--seed", "s"]).unwrap();
            assert!(is_identity(&cli.command, |action| matches!(
                action,
                Some(IdentityAction::Provision(p)) if p.number == 7
            )));
        }
    }

    #[test]
    fn number_and_seed_are_required() {
        assert!(parse(&["id", "provision", "--seed", "s"]).is_err());
        assert!(parse(&["id", "provision", "-n", "1"]).is_err());
        assert!(parse(&["id", "provision"]).is_err());
    }

    #[test]
    fn invalid_numbers_are_rejected() {
        for number in ["abc", "-1", "1.5", "", "18446744073709551616"] {
            assert!(
                parse(&["id", "provision", "-n", number, "--seed", "s"]).is_err(),
                "{number:?}"
            );
        }
    }

    #[test]
    fn no_other_identity_subcommands() {
        for name in ["show", "status", "generate", "get-public-key"] {
            assert!(parse(&["identity", name]).is_err(), "{name}");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn show_provision_and_erase_through_both_names() {
        let seed = TempFile::new("lifecycle.suri", Some(&format!("{DEV_PHRASE}\n")));
        let expected = rohi::derive_identity(DEV_PHRASE, 42).unwrap().public_key();
        for name in ["identity", "id"] {
            let outcome = run(Services::new(), &[name]).await;
            outcome.result.as_ref().unwrap();
            assert_eq!(outcome.stdout, "State       not provisioned\n");

            let args = [name, "provision", "-n", "42", "--seed", seed.path()];
            let outcome = run(outcome.services, &args).await;
            outcome.result.as_ref().unwrap();
            assert_eq!(stored_key(&outcome), Some(expected.to_bytes()), "{name}");
            assert!(
                outcome
                    .stdout
                    .starts_with("Identity provisioned\n\nAddress:    4")
            );
            assert!(outcome.stdout.contains(&format!("Public key: {expected}")));

            let outcome = run(outcome.services, &[name]).await;
            assert!(
                outcome
                    .stdout
                    .contains("State       provisioned\nAddress     4")
            );

            let outcome = run(outcome.services, &args).await;
            assert!(
                matches!(
                    outcome.device_error(),
                    Error::Device(rohi::model::ErrorCode::IdentityAlreadyProvisioned)
                ),
                "{name}"
            );
            assert!(
                outcome
                    .error()
                    .diagnostic()
                    .to_string()
                    .contains("hint: run `rohi identity erase`")
            );
            assert_eq!(stored_key(&outcome), Some(expected.to_bytes()));

            let outcome = run(outcome.services, &[name, "erase", "--yes"]).await;
            outcome.result.as_ref().unwrap();
            assert_eq!(stored_key(&outcome), None, "{name}");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unreadable_or_invalid_seed_never_reaches_the_device() {
        let outcome = run(
            Services::new(),
            &["id", "provision", "-n", "1", "--seed", "/nonexistent/x"],
        )
        .await;
        assert!(matches!(outcome.device_error(), Error::InvalidInput(_)));
        assert!(outcome.requests().is_empty());

        let seed = TempFile::new("bad.suri", Some("definitely not a secret uri"));
        let outcome = run(
            Services::new(),
            &["id", "provision", "-n", "1", "--seed", seed.path()],
        )
        .await;
        assert!(matches!(outcome.device_error(), Error::InvalidInput(_)));
        assert!(!outcome.error().to_string().contains("definitely"));
        assert!(outcome.requests().is_empty());
    }
}

// ---------------------------------------------------------------------------
// Connectivity
// ---------------------------------------------------------------------------

fn services_with_telemetry(host: &str, enabled: bool) -> Services {
    let services = Services::new();
    *services.connectivity.borrow_mut() = Some(rohi_connectivity::ConnectivityConfig {
        enabled,
        node_id: 42,
        interval_s: 60,
        host: rohi_connectivity::Host::new(host).unwrap(),
        port: 8080,
    });
    services
}

#[tokio::test(flavor = "current_thread")]
async fn config_shows_the_telemetry_section() {
    let outcome = run(
        services_with_telemetry("ingress.example", true),
        &["config", "connectivity"],
    )
    .await;
    outcome.result.as_ref().unwrap();
    assert_eq!(
        outcome.stdout,
        "\
Connectivity: enabled
  host:     ingress.example:8080
  node id:  42
  interval: 60 s
"
    );
    assert!(outcome.stderr.is_empty());

    let outcome = run(Services::new(), &["config", "connectivity"]).await;
    outcome.result.as_ref().unwrap();
    assert_eq!(outcome.stdout, "Connectivity: not configured\n");

    // Without a section: everything the device has.
    let outcome = run(services_with_telemetry("a.io", false), &["config"]).await;
    outcome.result.as_ref().unwrap();
    assert!(
        outcome
            .stdout
            .starts_with("Wi-Fi: not configured\nConnectivity: disabled\n")
    );

    let outcome = run(
        services_with_telemetry("a.io", true),
        &["-o", "json", "config", "connectivity"],
    )
    .await;
    assert_eq!(
        outcome.json()["connectivity"],
        json!({
            "enabled": true,
            "node_id": 42,
            "interval_s": 60,
            "host": "a.io",
            "port": 8080
        })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_device_without_telemetry_says_so() {
    let mut services = Services::new();
    services.capabilities = Capabilities::SENSORS.union(Capabilities::CONFIG);
    let outcome = run(services, &["config", "connectivity"]).await;
    assert!(matches!(
        outcome.device_error(),
        Error::Unsupported(rohi::model::Capability::Connectivity)
    ));

    let mut services = Services::new();
    services.capabilities = Capabilities::SENSORS.union(Capabilities::CONFIG);
    let outcome = run(services, &["connectivity", "set", "--disable"]).await;
    assert!(matches!(
        outcome.device_error(),
        Error::Unsupported(rohi::model::Capability::Connectivity)
    ));
    assert!(outcome.mutations().is_empty());

    // The plain `config` stays about what the device has.
    let mut services = Services::new();
    services.capabilities = Capabilities::SENSORS.union(Capabilities::CONFIG);
    let outcome = run(services, &["config"]).await;
    outcome.result.as_ref().unwrap();
    assert_eq!(outcome.stdout, "Wi-Fi: not configured\n");
}

#[tokio::test(flavor = "current_thread")]
async fn connectivity_set_creates_and_changes_the_configuration() {
    let outcome = run(
        Services::new(),
        &[
            "connectivity",
            "set",
            "--host",
            "ingress.example",
            "--node-id",
            "7",
            "--interval",
            "120",
            "--enable",
        ],
    )
    .await;
    outcome.result.as_ref().unwrap();
    assert_eq!(outcome.mutations(), ["set_connectivity"]);
    assert!(
        outcome
            .stdout
            .starts_with("Connectivity configuration stored")
    );
    let stored = outcome.services.connectivity.borrow().clone().unwrap();
    assert!(stored.enabled);
    assert_eq!(
        (stored.node_id, stored.interval_s, stored.port),
        (7, 120, 80)
    );
    assert_eq!(stored.host.as_str(), "ingress.example");

    // Omitted settings are kept.
    let outcome = run(
        services_with_telemetry("ingress.example", true),
        &["connectivity", "set", "--disable"],
    )
    .await;
    outcome.result.as_ref().unwrap();
    let stored = outcome.services.connectivity.borrow().clone().unwrap();
    assert!(!stored.enabled);
    assert_eq!((stored.node_id, stored.port), (42, 8080));

    let outcome = run(
        Services::new(),
        &["-o", "json", "connectivity", "set", "--host", "a.io"],
    )
    .await;
    assert_eq!(outcome.json(), json!({"schema_version": 1, "ok": true}));
}

#[tokio::test(flavor = "current_thread")]
async fn connectivity_set_rejects_bad_values_without_asking_the_device() {
    for args in [
        &["connectivity", "set", "--host", "bad host"][..],
        &["connectivity", "set", "--interval", "1"],
        &["connectivity", "set", "--interval", "86401"],
    ] {
        let outcome = run(Services::new(), args).await;
        assert!(
            matches!(outcome.device_error(), Error::InvalidInput(_)),
            "{args:?}"
        );
        assert!(outcome.requests().is_empty(), "{args:?}");
    }

    // The device validates the merged result: nothing stored, no host.
    let outcome = run(Services::new(), &["connectivity", "set", "--enable"]).await;
    assert!(matches!(
        outcome.device_error(),
        Error::Device(rohi::model::ErrorCode::InvalidArgument)
    ));
    assert!(outcome.services.connectivity.borrow().is_none());
}

#[test]
fn connectivity_set_needs_something_to_change() {
    use clap::Parser;
    assert!(rohi::cli::Cli::try_parse_from(["rohi", "connectivity", "set"]).is_err());
    assert!(
        rohi::cli::Cli::try_parse_from(["rohi", "connectivity", "set", "--enable", "--disable"])
            .is_err()
    );
    assert!(
        rohi::cli::Cli::try_parse_from(["rohi", "connectivity", "set", "--port", "0"]).is_err()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn backup_carries_the_telemetry_section() {
    let source = run(
        services_with_telemetry("ingress.example", true),
        &["backup", "export"],
    )
    .await;
    assert_eq!(
        source.json()["config"]["connectivity"],
        json!({
            "enabled": true,
            "node_id": 42,
            "interval_s": 60,
            "host": "ingress.example",
            "port": 8080
        })
    );
    let file = TempFile::new("telemetry.json", Some(&source.stdout));

    let outcome = run(
        Services::new(),
        &["-o", "json", "backup", "import", file.path(), "--dry-run"],
    )
    .await;
    assert_eq!(outcome.json()["changes"][0]["section"], "connectivity");
    assert!(outcome.mutations().is_empty());

    let outcome = run(Services::new(), &["backup", "import", file.path(), "--yes"]).await;
    outcome.result.as_ref().unwrap();
    assert_eq!(outcome.mutations(), ["set_connectivity"]);
    assert!(outcome.stderr.contains("Telemetry"), "{}", outcome.stderr);
    let stored = outcome.services.connectivity.borrow().clone().unwrap();
    assert_eq!(stored.host.as_str(), "ingress.example");
    assert_eq!(stored.node_id, 42);

    // The device already matches.
    let outcome = run(
        services_with_telemetry("ingress.example", true),
        &["backup", "import", file.path(), "--yes"],
    )
    .await;
    outcome.result.as_ref().unwrap();
    assert!(outcome.mutations().is_empty());

    // A section that is deliberately absent removes the stored one.
    let absent = TempFile::new(
        "absent.json",
        Some(
            r#"{"schema_version": 1, "source": {"firmware": "a", "board": "b"}, "config": {"connectivity": null}}"#,
        ),
    );
    let outcome = run(
        services_with_telemetry("ingress.example", true),
        &["backup", "import", absent.path(), "--yes"],
    )
    .await;
    outcome.result.as_ref().unwrap();
    assert!(outcome.services.connectivity.borrow().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn a_telemetry_backup_does_not_fit_a_device_without_telemetry() {
    let file = TempFile::new(
        "unsupported.json",
        Some(
            r#"{"schema_version": 1, "source": {"firmware": "a", "board": "b"}, "config": {"connectivity": null}}"#,
        ),
    );
    let mut services = Services::new();
    services.capabilities = Capabilities::SENSORS.union(Capabilities::CONFIG);
    let outcome = run(services, &["backup", "import", file.path(), "--yes"]).await;
    assert!(matches!(
        outcome.error(),
        CliError::Incompatible(sections) if sections == &["connectivity"]
    ));
    assert!(outcome.mutations().is_empty());
}
