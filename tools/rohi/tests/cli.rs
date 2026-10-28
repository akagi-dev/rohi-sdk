//! The `rohi` command line without Bluetooth: parsing, help, output contract,
//! the generic `status`, `config` and `action` commands, run through
//! `rohi::cli::execute` against the in-memory device.

mod common;

use std::cell::RefCell;
use std::io::{self, Write};
use std::rc::Rc;

use clap::{CommandFactory, Parser};
use common::*;
use rohi::cli::{Cli, CliError, Ctx, Io, PartialApply, execute};
use rohi::{Device, Error};
use rohi_config::Override;
use rohi_connectivity::ConnectivityConfig;
use rohi_control::{Controller, OpenPolicy};
use rohi_control_ble::ControlBle;
use rohi_net::WifiPassword;
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
    screen: bool,
    size: Option<(usize, usize)>,
    input: &'a [&'a str],
}

struct Outcome {
    result: Result<(), CliError>,
    stdout: String,
    stderr: String,
}

impl Outcome {
    fn ok(&self) -> &Self {
        if let Err(error) = &self.result {
            panic!("the command failed: {error}\nstderr: {}", self.stderr);
        }
        self
    }

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

    fn message(&self) -> String {
        self.error().diagnostic().to_string()
    }
}

fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(std::iter::once("rohi").chain(args.iter().copied()))
}

async fn run_with(bench: &Bench, args: &[&str], terminal: Terminal<'_>) -> Outcome {
    run_watching(bench, args, terminal, None).await
}

async fn run_watching(
    bench: &Bench,
    args: &[&str],
    terminal: Terminal<'_>,
    watch_limit: Option<usize>,
) -> Outcome {
    let cli = parse(args).unwrap();
    let (stdout, stderr) = (Buf::default(), Buf::default());
    let io = Io::scripted(
        stdout.clone(),
        stderr.clone(),
        terminal.interactive,
        terminal.input,
    )
    .with_screen(terminal.screen);
    let io = match terminal.size {
        Some((columns, rows)) => io.with_size(columns, rows),
        None => io,
    };
    let mut ctx = Ctx::new(io, cli.output);
    ctx.color(cli.color);
    ctx.uri = Some("ble://BENCH-1234".into());
    ctx.watch_limit = watch_limit;

    let (client, device) = link(244);
    let controller = Controller::new(backend(bench), OpenPolicy);
    let mut server = ControlBle::new(device, &controller);
    let command = cli.command;
    let result = tokio::select! {
        () = async { server.run().await } => unreachable!(),
        result = async {
            let mut device = Device::new(client);
            execute(&mut device, command, &mut ctx).await
        } => result,
    };
    Outcome {
        result,
        stdout: stdout.text(),
        stderr: stderr.text(),
    }
}

async fn run(bench: &Bench, args: &[&str]) -> Outcome {
    run_with(bench, args, Terminal::default()).await
}

/// The `--help` page of a command: clap's, or for `config` and `action` the
/// static one printed before any device is contacted.
fn help(args: &[&str]) -> String {
    let argv = std::iter::once("rohi")
        .chain(args.iter().copied())
        .chain(["--help"]);
    match Cli::try_parse_from(argv) {
        Err(error) => error.to_string(),
        Ok(cli) => rohi::cli::static_help(&cli.command)
            .unwrap_or_else(|| panic!("`{args:?} --help` is not a static page")),
    }
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

fn temp_file(name: &str, content: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rohi-cli-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    path
}

fn path(path: &std::path::Path) -> &str {
    path.to_str().unwrap()
}

// ---------------------------------------------------------------------------
// Interface
// ---------------------------------------------------------------------------

#[test]
fn help_texts_are_snapshots() {
    let pages: &[&[&str]] = &[
        &[],
        &["scan"],
        &["status"],
        &["config"],
        &["config", "reset"],
        &["config", "export"],
        &["config", "diff"],
        &["config", "apply"],
        &["factory-reset"],
        &["reboot"],
        &["action"],
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
    assert!(found.len() > 25, "{found:#?}");
    for example in found {
        let argv: Vec<_> = example.split_whitespace().collect();
        if let Err(error) = Cli::try_parse_from(argv) {
            panic!("`{example}` does not parse: {error}");
        }
    }
}

#[test]
fn the_command_tree_has_no_module_specific_commands() {
    // No compile-time list of sections, services or modules: only the
    // generic verbs.
    let names: Vec<_> = Cli::command()
        .get_subcommands()
        .map(|command| command.get_name().to_owned())
        .collect();
    assert_eq!(
        names,
        [
            "scan",
            "status",
            "config",
            "factory-reset",
            "reboot",
            "action"
        ]
    );
    for gone in ["wifi", "connectivity", "backup", "sensors", "time", "info"] {
        let error = parse(&[gone]).err().unwrap();
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::InvalidSubcommand,
            "{gone}"
        );
    }
    assert!(parse(&["-o", "json", "status"]).is_ok());
    assert!(parse(&["-o", "yaml", "status"]).is_err());
}

#[test]
fn usage_errors_have_exit_code_2() {
    for args in [
        &["bogus"][..],
        &["config", "reset"],
        &["config", "diff"],
        &["config", "wifi", "--bogus"],
        &["action", "wifi", "scan", "--bogus"],
        &["status", "--interval", "5"],
    ] {
        assert_eq!(parse(args).err().unwrap().exit_code(), 2, "{args:?}");
    }
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn status_is_one_compact_summary_of_all_services() {
    let bench = Bench::new();
    let outcome = run(&bench, &["status"]).await;
    outcome.ok();
    assert_eq!(
        outcome.stdout,
        "\
Device:  fake-1.0.0 (bench-esp32c3)
Uptime:  1m 30s
Health:  ok

SERVICE   STATE     HEALTH  READY  SINCE
radio     running   ok      yes    0ms
identity  stopped   -       no     0ms
location  disabled  -       no     -
"
    );
    assert!(outcome.stderr.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn a_service_in_detail_shows_values_units_and_issues() {
    let bench = Bench::new();
    bench.radio.degraded.set(true);
    let outcome = run(&bench, &["status", "radio"]).await;
    outcome.ok();
    assert!(
        outcome
            .stdout
            .starts_with("Service:  radio\nState:    running\nHealth:   degraded\nReady:    no\n"),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("ssid  office"),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("rssi  -61 dBm"),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("WARNING  link_lost"),
        "{}",
        outcome.stdout
    );

    let outcome = run(&bench, &["status", "missing"]).await;
    assert!(matches!(
        outcome.device_error(),
        Error::Device(rohi::model::ErrorCode::NotFound)
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn status_json_is_keyed_by_names_and_carries_no_protocol_types() {
    let bench = Bench::new();
    let all = run(&bench, &["-o", "json", "status"]).await;
    let json = all.ok().json();
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["device"]["firmware"], "fake-1.0.0");
    assert_eq!(json["health"], "ok");
    assert_eq!(json["services"][0]["service"], "radio");
    assert_eq!(json["services"][0]["state"], "running");
    assert_eq!(json["services"][0]["ready"], true);

    let one = run(&bench, &["-o", "json", "status", "radio"]).await;
    let json = one.ok().json();
    assert_eq!(json["service"]["service"], "radio");
    assert_eq!(json["values"]["ssid"], "office");
    assert_eq!(json["values"]["rssi"], -61);
    assert_eq!(json["units"]["rssi"], "dBm");
    assert_eq!(json["issues"], json!([]));
}

#[tokio::test(flavor = "current_thread")]
async fn status_shows_the_sensor_id_once_an_identity_is_provisioned() {
    use sp_core::crypto::Ss58Codec;

    let bench = Bench::new();
    let outcome = run(&bench, &["status"]).await;
    assert!(
        !outcome.ok().stdout.contains("SensorId"),
        "{}",
        outcome.stdout
    );

    let derived = rohi::derive_identity(DEV_PHRASE, 7).unwrap();
    let expected = sp_core::ed25519::Public::from_raw(derived.public_key().to_bytes())
        .to_ss58check_with_version(
            sp_core::crypto::Ss58AddressFormatRegistry::RobonomicsAccount.into(),
        );
    let secret = hex(derived.secret().expose_secret());
    let terminal = Terminal {
        interactive: false,
        input: &[&secret],
        ..Default::default()
    };
    run_with(
        &bench,
        &["action", "identity", "provision", "secret=-"],
        terminal,
    )
    .await
    .ok();

    let outcome = run(&bench, &["status"]).await;
    let lines: Vec<&str> = outcome.ok().stdout.lines().collect();
    assert_eq!(lines[0], format!("SensorId:  {expected}"));
    assert_eq!(lines[1], "Device:    fake-1.0.0 (bench-esp32c3)");
    assert_eq!(lines[2], "Uptime:    1m 30s");

    let json = run(&bench, &["-o", "json", "status"]).await.ok().json();
    assert_eq!(json["sensor_id"], expected);
}

#[tokio::test(flavor = "current_thread")]
async fn a_watch_prints_again_only_when_the_status_changed() {
    let bench = Bench::new();
    // Unchanged status: the first document is printed, the rest is silent
    // until something changes. The limit stops the loop after the change.
    let toggler = async {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        bench.radio.degraded.set(true);
    };
    let (outcome, ()) = tokio::join!(
        run_watching(
            &bench,
            &["status", "--watch", "--interval", "0"],
            Terminal::default(),
            Some(2),
        ),
        toggler
    );
    outcome.ok();
    let documents = outcome.stdout.matches("Device:").count();
    assert_eq!(documents, 2, "{}", outcome.stdout);
    assert!(outcome.stdout.contains("degraded"), "{}", outcome.stdout);
}

#[tokio::test(flavor = "current_thread")]
async fn color_is_applied_only_when_asked_for_and_never_to_json() {
    let bench = Bench::new();
    let plain = run(&bench, &["status"]).await;
    assert!(!plain.ok().stdout.contains('\x1b'), "{:?}", plain.stdout);

    let never = run(&bench, &["--color", "never", "status"]).await;
    assert_eq!(never.ok().stdout, plain.stdout);

    let colored = run(&bench, &["--color", "always", "status"]).await;
    let text = &colored.ok().stdout;
    // Health words take their color; padding stays outside of the escapes so
    // the columns line up.
    assert!(text.contains("\x1b[32mrunning\x1b[0m  "), "{text:?}");
    assert!(text.contains("\x1b[1mSERVICE\x1b[0m"), "{text:?}");
    let stripped: String = text
        .split('\x1b')
        .enumerate()
        .map(|(index, part)| match (index, part.find('m')) {
            (0, _) => part,
            (_, Some(end)) => &part[end + 1..],
            _ => part,
        })
        .collect();
    assert_eq!(stripped, plain.stdout);

    let json = run(&bench, &["--color", "always", "-o", "json", "status"]).await;
    assert!(!json.ok().stdout.contains('\x1b'), "{:?}", json.stdout);
}

#[tokio::test(flavor = "current_thread")]
async fn a_watch_on_a_terminal_redraws_in_place() {
    let bench = Bench::new();
    let outcome = run_watching(
        &bench,
        &["status", "--watch", "--interval", "0"],
        Terminal {
            screen: true,
            ..Terminal::default()
        },
        Some(3),
    )
    .await;
    outcome.ok();
    // Every refresh homes the cursor, even when nothing changed.
    assert_eq!(outcome.stdout.matches("\x1b[H").count(), 3);
    assert_eq!(outcome.stdout.matches("Device:").count(), 3);
    assert!(outcome.stdout.ends_with("\x1b[J"), "{:?}", outcome.stdout);
}

#[tokio::test(flavor = "current_thread")]
async fn a_watch_redraws_in_place_only_while_it_fits_the_terminal() {
    let bench = Bench::new();
    let bench = &bench;
    let watch = |size, limit| async move {
        run_watching(
            bench,
            &["status", "--watch", "--interval", "0"],
            Terminal {
                screen: true,
                size,
                ..Terminal::default()
            },
            Some(limit),
        )
        .await
    };

    // Plenty of room: the last line has no newline, so a full screen would
    // not scroll either.
    let outcome = watch(Some((200, 100)), 2).await;
    outcome.ok();
    assert_eq!(outcome.stdout.matches("\x1b[H").count(), 2);
    assert!(
        !outcome.stdout.contains("\x1b[K\n\x1b[J"),
        "{:?}",
        outcome.stdout
    );

    // Too few rows: ordinary output.
    let outcome = watch(Some((200, 2)), 1).await;
    assert!(!outcome.stdout.contains("\x1b[H"), "{:?}", outcome.stdout);
    assert!(outcome.stdout.contains("Device:"), "{:?}", outcome.stdout);

    // Wrapping makes lines take more than one row.
    let outcome = watch(Some((4, 20)), 1).await;
    assert!(!outcome.stdout.contains("\x1b[H"), "{:?}", outcome.stdout);
}

// ---------------------------------------------------------------------------
// config
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn sections_are_listed_and_described_from_the_device() {
    let bench = Bench::new();
    let outcome = run(&bench, &["config"]).await;
    outcome.ok();
    assert_eq!(
        outcome.stdout,
        "SECTION       VERSION\nwifi          1\nconnectivity  1\ngadget        3\nlocation      1\n"
    );
    let outcome = run(&bench, &["-o", "json", "config"]).await;
    assert_eq!(outcome.ok().json()["sections"][2]["name"], "gadget");

    // The help of a section is generated from the schema of the device.
    let outcome = run(&bench, &["config", "gadget", "--help"]).await;
    assert_eq!(
        outcome.ok().stdout,
        "\
Usage:
  rohi config gadget [FIELD=VALUE...]

Fields:
  label   string
  level   u64 %
  mode    enum (fast|slow), nullable
  serial  string, read-only

Special values:
  FIELD=none  set a nullable field to no value, whatever the default is
  FIELD=null  clear the override and use the field default

Without FIELD=VALUE the effective configuration is shown: the defaults of the
firmware with your overrides applied. `rohi config reset gadget` removes
all overrides.
"
    );

    // Secrets get their own line in the help.
    let outcome = run(&bench, &["config", "wifi", "-h"]).await;
    let help = outcome.ok().stdout.clone();
    assert!(help.contains("password"), "{help}");
    assert!(help.contains("string, secret, nullable"), "{help}");
    assert!(help.contains("SECRET=-"), "{help}");
    assert!(help.contains("scan_timeout"), "{help}");

    // JSON gets the schema.
    let outcome = run(&bench, &["-o", "json", "config", "gadget", "--help"]).await;
    let json = outcome.ok().json();
    assert_eq!(json["fields"][1]["field"]["name"], "level");

    // The dynamic help needs the device: an unknown section is an error.
    let outcome = run(&bench, &["config", "missing", "--help"]).await;
    assert!(matches!(
        outcome.device_error(),
        Error::Device(rohi::model::ErrorCode::NotFound)
    ));
    let outcome = run(&bench, &["config", "missing"]).await;
    assert!(matches!(
        outcome.device_error(),
        Error::Device(rohi::model::ErrorCode::NotFound)
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn the_static_help_of_config_and_action_needs_no_device() {
    let bench = Bench::new();
    for command in ["config", "action"] {
        let outcome = run(&bench, &[command, "--help"]).await;
        let text = outcome.ok().stdout.clone();
        assert!(text.contains(&format!("rohi {command}")), "{text}");
        assert!(text.contains("Examples:"), "{text}");
    }
    assert_eq!(bench.writes(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn setting_fields_shows_the_effective_configuration_the_device_reports() {
    let bench = Bench::new();
    let outcome = run(
        &bench,
        &["config", "gadget", "label=lamp", "level=70", "mode=slow"],
    )
    .await;
    outcome.ok();
    assert_eq!(
        outcome.stdout,
        "label   lamp\nlevel   70 %\nmode    slow\nserial  SN-1\n\nA restart is required for the change to take effect\n"
    );
    assert_eq!(
        bench.gadget.effective::<Gadget>(),
        Gadget {
            label: "lamp".into(),
            level: 70,
            mode: Some(1)
        }
    );

    let outcome = run(&bench, &["config", "gadget"]).await;
    assert_eq!(
        outcome.ok().stdout,
        "label   lamp\nlevel   70 %\nmode    slow\nserial  SN-1\n"
    );
    let outcome = run(&bench, &["-o", "json", "config", "gadget"]).await;
    let json = outcome.ok().json();
    assert_eq!(json["values"]["level"], 70);
    assert_eq!(json["values"]["mode"], "slow");
    assert_eq!(json["overridden"], json!(["label", "level", "mode"]));
    assert_eq!(json["section"]["name"], "gadget");
    assert!(json.get("configured").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn a_field_that_is_not_overridden_shows_its_default_and_null_restores_it() {
    let bench = Bench::new();
    // Nothing is overridden: the effective configuration is the defaults, a
    // field whose default is none is `<unset>`.
    let outcome = run(&bench, &["config", "gadget"]).await;
    assert_eq!(
        outcome.ok().stdout,
        "label   \nlevel   50 %\nmode    <unset>\nserial  SN-1\n"
    );
    let outcome = run(&bench, &["-o", "json", "config", "gadget"]).await;
    let json = outcome.ok().json();
    assert_eq!(json["values"]["mode"], Value::Null);
    assert_eq!(json["overridden"], json!([]));
    assert!(bench.gadget.value.borrow().is_none());

    run(
        &bench,
        &["config", "gadget", "label=lamp", "level=70", "mode=slow"],
    )
    .await
    .ok();

    // `null` removes the override: the field follows the default again.
    let outcome = run(&bench, &["config", "gadget", "level=null"]).await;
    assert_eq!(
        outcome.ok().stdout,
        "label   lamp\nlevel   50 %\nmode    slow\nserial  SN-1\n\nA restart is required for the change to take effect\n"
    );
    let stored = bench.gadget.value.borrow().clone().unwrap();
    assert_eq!(stored.level, Override::Default);
    assert_eq!(stored.mode, Override::Value(Some(1)));

    // For a field whose default is none it is the same: no value.
    let outcome = run(&bench, &["config", "gadget", "mode=null"]).await;
    assert!(
        outcome.ok().stdout.contains("mode    <unset>\n"),
        "{}",
        outcome.stdout
    );
    assert_eq!(
        bench.gadget.value.borrow().clone().unwrap().mode,
        Override::Default
    );

    // A required field can be cleared too; the section is then not valid, but
    // it has no override left except the label.
    let outcome = run(&bench, &["config", "gadget", "label=null"]).await;
    assert!(
        outcome.ok().stdout.contains("label   \n"),
        "{}",
        outcome.stdout
    );
    assert!(bench.gadget.value.borrow().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn setting_and_clearing_fields_can_be_combined_in_one_patch() {
    let bench = Bench::new();
    run(
        &bench,
        &["config", "gadget", "label=lamp", "level=70", "mode=slow"],
    )
    .await
    .ok();
    let outcome = run(&bench, &["config", "gadget", "level=null", "mode=fast"]).await;
    outcome.ok();
    let stored = bench.gadget.value.borrow().clone().unwrap();
    assert_eq!(stored.label, Override::Value("lamp".into()));
    assert_eq!(stored.level, Override::Default);
    assert_eq!(stored.mode, Override::Value(Some(0)));
}

#[tokio::test(flavor = "current_thread")]
async fn bad_input_is_refused_before_anything_is_stored() {
    let bench = Bench::new();
    let cases: &[(&[&str], &str)] = &[
        (
            &["config", "gadget", "level=lots"],
            "level: expected a non-negative integer, got `lots`",
        ),
        (
            &["config", "gadget", "mode=medium"],
            "mode: expected one of fast, slow, got `medium`",
        ),
        (
            &["config", "gadget", "colour=red"],
            "gadget has no field `colour`; its fields are: label, level, mode, serial",
        ),
        (
            &["config", "gadget", "serial=X"],
            "gadget.serial is read-only",
        ),
        (
            &["config", "gadget", "serial=null"],
            "gadget.serial is read-only",
        ),
        (&["config", "gadget", "label"], "`label` is not NAME=VALUE"),
        (
            &["config", "gadget", "label=a", "label=b"],
            "label is given twice",
        ),
        (
            &["config", "gadget", "--dry-run"],
            "--dry-run needs FIELD=VALUE",
        ),
        (&["config", "--dry-run"], "name the section"),
    ];
    for (args, message) in cases {
        let outcome = run(&bench, args).await;
        assert!(
            matches!(outcome.error(), CliError::Usage(_)),
            "{args:?}: {}",
            outcome.message()
        );
        assert!(
            outcome.message().contains(message),
            "{args:?}: {}",
            outcome.message()
        );
    }
    // The device refuses what the schema cannot express: nothing is stored.
    let outcome = run(&bench, &["config", "gadget", "level=500"]).await;
    assert!(matches!(
        outcome.device_error(),
        Error::Device(rohi::model::ErrorCode::InvalidArgument)
    ));
    let outcome = run(&bench, &["config", "gadget", "mode=fast"]).await;
    assert!(
        matches!(outcome.error(), CliError::Device(_)),
        "a label is needed"
    );
    assert_eq!(bench.writes(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn dry_run_validates_without_storing() {
    let bench = Bench::new();
    let outcome = run(&bench, &["config", "gadget", "label=lamp", "--dry-run"]).await;
    assert_eq!(
        outcome.ok().stdout,
        "`gadget` is valid; nothing was changed\n"
    );
    assert_eq!(bench.writes(), 0);
    let outcome = run(&bench, &["config", "gadget", "mode=fast", "--dry-run"]).await;
    assert!(matches!(outcome.error(), CliError::Device(_)));
}

#[tokio::test(flavor = "current_thread")]
async fn secrets_never_travel_on_the_command_line_or_come_back() {
    let bench = Bench::new();
    let outcome = run(
        &bench,
        &["config", "wifi", "ssid=office", "password=hunter2hunter2"],
    )
    .await;
    assert!(matches!(outcome.error(), CliError::Usage(_)));
    assert!(
        outcome.message().contains("password=-"),
        "{}",
        outcome.message()
    );
    assert!(!outcome.message().contains("hunter2"));
    assert_eq!(bench.writes(), 0);

    // From stdin.
    let terminal = Terminal {
        input: &["hunter2hunter2"],
        ..Default::default()
    };
    let outcome = run_with(
        &bench,
        &["config", "wifi", "ssid=office", "password=-"],
        terminal,
    )
    .await;
    outcome.ok();
    assert!(!outcome.stdout.contains("hunter2") && !outcome.stderr.contains("hunter2"));
    assert!(
        outcome.stdout.contains("password         <redacted>\n"),
        "{}",
        outcome.stdout
    );

    // On a terminal the user is asked without echo.
    let terminal = Terminal {
        interactive: true,
        input: &["hunter2hunter3"],
        ..Default::default()
    };
    let outcome = run_with(&bench, &["config", "wifi", "password=-"], terminal).await;
    outcome.ok();
    assert_eq!(outcome.stderr, "password: \n");
    let stored = bench.wifi.value.borrow().clone().unwrap();
    assert_eq!(
        stored.password,
        Override::Value(Some(WifiPassword::from_bytes(b"hunter2hunter3").unwrap()))
    );

    // Reading shows the secret as redacted, in text and in JSON.
    let outcome = run(&bench, &["config", "wifi"]).await;
    assert_eq!(
        outcome.ok().stdout,
        "ssid             office\npassword         <redacted>\n\
scan_timeout     20000 ms\n\
connect_timeout  30000 ms\n\
backoff_initial  1000 ms\n\
backoff_max      60000 ms\n\
link_refresh     10000 ms\n"
    );
    let outcome = run(&bench, &["-o", "json", "config", "wifi"]).await;
    let json = outcome.ok().json();
    assert_eq!(json["values"]["password"], json!({ "secret": true }));
    assert!(!outcome.stdout.contains("hunter2"));

    // Empty stdin is an error, never an empty secret.
    let outcome = run(&bench, &["config", "wifi", "password=-"]).await;
    assert!(matches!(outcome.error(), CliError::PasswordRequired { .. }));

    // An empty line is a value for the device to judge, not a missing one.
    let terminal = Terminal {
        input: &[""],
        ..Default::default()
    };
    let outcome = run_with(&bench, &["config", "wifi", "password=-"], terminal).await;
    assert!(
        !matches!(outcome.result, Err(CliError::PasswordRequired { .. })),
        "{}",
        outcome.stderr
    );

    // `null` is not a secret: it removes the override, no input is read.
    let outcome = run(&bench, &["config", "wifi", "password=null"]).await;
    assert!(
        outcome.ok().stdout.contains("password         <unset>\n"),
        "{}",
        outcome.stdout
    );
    assert_eq!(
        bench.wifi.value.borrow().clone().unwrap().password,
        Override::Default
    );
}

#[tokio::test(flavor = "current_thread")]
async fn reset_asks_only_where_somebody_can_answer() {
    let bench = Bench::new();
    run(&bench, &["config", "gadget", "label=lamp"]).await.ok();

    let outcome = run(&bench, &["config", "reset", "gadget"]).await;
    assert!(matches!(outcome.error(), CliError::ConfirmationRequired));
    assert!(bench.gadget.value.borrow().is_some());

    let terminal = Terminal {
        interactive: true,
        input: &["n"],
        ..Default::default()
    };
    let outcome = run_with(&bench, &["config", "reset", "gadget"], terminal).await;
    assert!(matches!(outcome.error(), CliError::Cancelled));
    assert!(bench.gadget.value.borrow().is_some());

    let terminal = Terminal {
        interactive: true,
        input: &["y"],
        ..Default::default()
    };
    let outcome = run_with(&bench, &["config", "reset", "gadget"], terminal).await;
    outcome.ok();
    assert!(
        outcome
            .stderr
            .contains("Every override of the `gadget` section"),
        "{}",
        outcome.stderr
    );
    assert_eq!(
        outcome.stdout,
        "`gadget` reset to the defaults (revision 1)\nA restart is required for the change to take effect\n"
    );
    assert!(bench.gadget.value.borrow().is_none());

    run(&bench, &["config", "gadget", "label=lamp"]).await.ok();
    run(&bench, &["config", "reset", "gadget", "--yes"])
        .await
        .ok();
    assert!(bench.gadget.value.borrow().is_none());

    // The name is checked before anybody is asked.
    let outcome = run(&bench, &["config", "reset", "missing", "--yes"]).await;
    assert!(matches!(
        outcome.device_error(),
        Error::Device(rohi::model::ErrorCode::NotFound)
    ));
}

// ---------------------------------------------------------------------------
// config files
// ---------------------------------------------------------------------------

async fn configured() -> Bench {
    let bench = Bench::new();
    run(
        &bench,
        &[
            "config",
            "connectivity",
            "host=old.example.org",
            "port=80",
            "interval_s=60",
            "enabled=true",
        ],
    )
    .await
    .ok();
    let terminal = Terminal {
        input: &["hunter2hunter2"],
        ..Default::default()
    };
    run_with(
        &bench,
        &["config", "wifi", "ssid=office", "password=-"],
        terminal,
    )
    .await
    .ok();
    run(&bench, &["config", "gadget", "label=lamp", "mode=fast"])
        .await
        .ok();
    bench
}

#[tokio::test(flavor = "current_thread")]
async fn export_writes_the_explicit_overrides_without_secrets_or_read_only_fields() {
    let bench = configured().await;
    let outcome = run(&bench, &["config", "export"]).await;
    outcome.ok();
    let json = outcome.json();
    // Only what was set: the timing of the Wi-Fi and the other fields that
    // follow the defaults are not frozen into the file. A value that was set
    // explicitly stays, also when it equals the default (the port).
    assert_eq!(
        json,
        json!({
            "wifi": { "ssid": "office" },
            "connectivity": { "enabled": true, "interval_s": 60, "host": "old.example.org", "port": 80 },
            "gadget": { "label": "lamp", "mode": "fast" },
        })
    );
    assert!(!outcome.stdout.contains("hunter2"));
    assert!(!outcome.stdout.contains("serial"));
    assert!(!outcome.stdout.contains("scan_timeout"));

    // A section without overrides is not exported.
    let outcome = run(&Bench::new(), &["config", "export"]).await;
    assert_eq!(outcome.ok().json(), json!({}));

    let file = temp_file("exported.json", "");
    let outcome = run(&bench, &["config", "export", path(&file)]).await;
    assert_eq!(
        outcome.ok().stdout,
        format!("Exported 3 section(s) to {}\n", file.display())
    );
    let written: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(written["wifi"]["ssid"], "office");
    assert!(written["wifi"].get("password").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn diff_shows_what_applying_would_change_and_changes_nothing() {
    let bench = configured().await;
    let writes = bench.writes();
    let file = temp_file(
        "diff.json",
        r#"{
            "connectivity": { "host": "new.example.org", "interval_s": 120, "port": 80 },
            "gadget": { "mode": null, "level": 20 },
            "wifi": null
        }"#,
    );
    let outcome = run(&bench, &["config", "diff", path(&file)]).await;
    outcome.ok();
    assert_eq!(
        outcome.stdout,
        "\
connectivity:
  ~ host            old.example.org -> new.example.org
  ~ interval_s      60 s -> 120 s
gadget:
  + level           20 %
  - mode            fast -> (default)
wifi:
  reset section
"
    );
    assert_eq!(bench.writes(), writes);

    // The same file against a device that already matches.
    let same = temp_file("same.json", r#"{ "connectivity": { "port": 80 } }"#);
    let outcome = run(&bench, &["config", "diff", path(&same)]).await;
    assert_eq!(outcome.ok().stdout, "No differences\n");

    // A value that equals the default is still a change when it is not an
    // override yet, and clearing a field that is not overridden is none.
    let pin = temp_file(
        "pin.json",
        r#"{ "gadget": { "level": 50, "mode": null, "label": "lamp" } }"#,
    );
    let outcome = run(&bench, &["config", "diff", path(&pin)]).await;
    assert_eq!(
        outcome.ok().stdout,
        "gadget:\n  + level           50 %\n  - mode            fast -> (default)\n"
    );
    let clear_default = temp_file("clear.json", r#"{ "gadget": { "level": null } }"#);
    let outcome = run(&bench, &["config", "diff", path(&clear_default)]).await;
    assert_eq!(outcome.ok().stdout, "No differences\n");

    let outcome = run(&bench, &["-o", "json", "config", "diff", path(&file)]).await;
    let json = outcome.ok().json();
    assert_eq!(json["differences"][0]["section"], "connectivity");
    assert_eq!(json["differences"][2]["reset"], true);
}

#[tokio::test(flavor = "current_thread")]
async fn apply_merges_unsets_and_resets_with_one_confirmation() {
    let bench = configured().await;
    let file = temp_file(
        "apply.json",
        r#"{
            "connectivity": { "host": "new.example.org", "port": 443 },
            "gadget": { "mode": null },
            "wifi": null
        }"#,
    );
    let terminal = Terminal {
        interactive: true,
        input: &["y"],
        ..Default::default()
    };
    let outcome = run_with(&bench, &["config", "apply", path(&file)], terminal).await;
    outcome.ok();
    assert!(
        outcome.stderr.contains("This changes the device"),
        "{}",
        outcome.stderr
    );
    assert!(
        outcome
            .stdout
            .starts_with("Applied: connectivity, gadget, wifi\n"),
        "{}",
        outcome.stdout
    );
    // The gadget needs a restart.
    assert!(outcome.stdout.contains("restart"), "{}", outcome.stdout);

    let connectivity = bench.telemetry.effective::<ConnectivityConfig>();
    assert_eq!(connectivity.host.as_str(), "new.example.org");
    assert_eq!(connectivity.port, 443);
    // Missing fields are left alone.
    assert_eq!(connectivity.interval_s, 60);
    assert!(connectivity.enabled);
    // `null` removed the override of the mode: it follows the default.
    assert_eq!(
        bench.gadget.value.borrow().clone().unwrap().mode,
        Override::Default
    );
    assert!(bench.wifi.value.borrow().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn apply_validates_every_section_before_changing_any() {
    let bench = configured().await;
    let writes = bench.writes();
    // The first section is fine, the second one is out of range for the
    // device: nothing may be applied.
    let file = temp_file(
        "invalid.json",
        r#"{ "connectivity": { "host": "new.example.org" }, "gadget": { "level": 101 } }"#,
    );
    let outcome = run(&bench, &["config", "apply", path(&file), "--yes"]).await;
    assert!(
        matches!(outcome.error(), CliError::Device(_)),
        "{}",
        outcome.message()
    );
    assert_eq!(bench.writes(), writes);
    assert_eq!(
        bench
            .telemetry
            .effective::<ConnectivityConfig>()
            .host
            .as_str(),
        "old.example.org"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn apply_dry_run_and_non_interactive_confirmation() {
    let bench = configured().await;
    let writes = bench.writes();
    let file = temp_file("dry.json", r#"{ "connectivity": { "port": 8080 } }"#);

    let outcome = run(&bench, &["config", "apply", path(&file), "--dry-run"]).await;
    outcome.ok();
    assert!(
        outcome
            .stdout
            .starts_with("The file is valid; nothing was changed"),
        "{}",
        outcome.stdout
    );
    assert_eq!(bench.writes(), writes);

    let outcome = run(&bench, &["config", "apply", path(&file)]).await;
    assert!(matches!(outcome.error(), CliError::ConfirmationRequired));
    assert_eq!(bench.writes(), writes);

    let outcome = run(&bench, &["config", "apply", path(&file), "--yes"]).await;
    outcome.ok();
    assert_eq!(bench.telemetry.effective::<ConnectivityConfig>().port, 8080);

    // A second application has nothing to do.
    let outcome = run(&bench, &["config", "apply", path(&file), "--yes"]).await;
    assert_eq!(outcome.ok().stdout, "Nothing to change\n");
}

#[tokio::test(flavor = "current_thread")]
async fn bad_files_never_reach_the_device() {
    let bench = configured().await;
    let writes = bench.writes();
    let cases: &[(&str, &str)] = &[
        ("[]", "must be a JSON object"),
        ("{", "not valid JSON"),
        (
            r#"{ "mqtt": {} }"#,
            "the device has no section `mqtt`; its sections are: wifi, connectivity, gadget, location",
        ),
        (
            r#"{ "gadget": { "colour": 1 } }"#,
            "gadget has no field `colour`",
        ),
        (
            r#"{ "gadget": { "serial": "x" } }"#,
            "gadget.serial is read-only",
        ),
        (
            r#"{ "wifi": { "password": "hunter2hunter2" } }"#,
            "secret and cannot be part of a file",
        ),
        (
            r#"{ "gadget": { "level": "high" } }"#,
            "level: expected a non-negative integer",
        ),
        (
            r#"{ "gadget": { "mode": "medium" } }"#,
            "mode: expected one of fast, slow",
        ),
        (
            r#"{ "gadget": 5 }"#,
            "gadget must be an object of fields or null",
        ),
    ];
    for (content, message) in cases {
        let file = temp_file("bad.json", content);
        for command in ["apply", "diff"] {
            let outcome = run(
                &bench,
                &["config", command, path(&file), "--yes"][..if command == "diff" { 3 } else { 4 }],
            )
            .await;
            assert!(
                matches!(outcome.error(), CliError::File(_)),
                "{content}: {}",
                outcome.message()
            );
            assert!(
                outcome.message().contains(message),
                "{content}: {}",
                outcome.message()
            );
        }
    }
    let outcome = run(&bench, &["config", "diff", "/nonexistent/rohi.json"]).await;
    assert!(
        outcome.message().contains("cannot read"),
        "{}",
        outcome.message()
    );
    assert_eq!(bench.writes(), writes);
}

#[test]
fn a_partial_apply_names_what_was_and_was_not_applied() {
    let error = CliError::PartialApply(Box::new(PartialApply {
        applied: vec!["wifi".into()],
        failed: "connectivity".into(),
        pending: vec!["gadget".into()],
        source: Error::Timeout,
    }));
    let text = error.diagnostic().to_string();
    assert!(text.contains("failed: connectivity"), "{text}");
    assert!(text.contains("applied:\n  wifi"), "{text}");
    assert!(text.contains("not applied:\n  gadget"), "{text}");
    assert!(text.contains("atomic per section only"), "{text}");
}

// ---------------------------------------------------------------------------
// action
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn actions_are_listed_described_and_run_from_the_schema() {
    let bench = Bench::new();
    let outcome = run(&bench, &["action"]).await;
    outcome.ok();
    assert!(
        outcome
            .stdout
            .starts_with("SERVICE   ACTION     PRIVILEGED\n"),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("radio     reconnect  yes"),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("radio     scan       no"),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("identity  provision  yes"),
        "{}",
        outcome.stdout
    );

    // The actions of one service with their arguments.
    let outcome = run(&bench, &["action", "radio"]).await;
    outcome.ok();
    assert!(
        outcome
            .stdout
            .starts_with("ACTION     ARGS     PRIVILEGED\n"),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("tune       channel  no"),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("scan       -        no"),
        "{}",
        outcome.stdout
    );
    assert!(!outcome.stdout.contains("provision"), "{}", outcome.stdout);
    let outcome = run(&bench, &["action", "missing"]).await;
    assert!(matches!(
        outcome.device_error(),
        Error::Device(rohi::model::ErrorCode::NotFound)
    ));

    // The help of an action is generated from its schema.
    let outcome = run(&bench, &["action", "radio", "scan", "--help"]).await;
    outcome.ok();
    assert!(
        outcome
            .stdout
            .starts_with("Usage:\n  rohi action radio scan\n"),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("Results (one record each):"),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("security  enum (open|wpa2)"),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.ends_with("Privileged: no\n"),
        "{}",
        outcome.stdout
    );
    let outcome = run(&bench, &["action", "radio", "tune", "-h"]).await;
    assert!(
        outcome
            .ok()
            .stdout
            .starts_with("Usage:\n  rohi action radio tune channel=VALUE\n"),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome
            .stdout
            .contains("Arguments (all required):\n  channel  u64"),
        "{}",
        outcome.stdout
    );
    let outcome = run(&bench, &["-o", "json", "action", "radio", "scan", "--help"]).await;
    let json = outcome.ok().json();
    assert_eq!(json["action"]["name"], "scan");
    // Asking for help runs nothing.
    assert!(bench.radio.calls.borrow().is_empty());

    let outcome = run(&bench, &["action", "radio", "scan"]).await;
    assert_eq!(
        outcome.ok().stdout,
        "SSID    RSSI     SECURITY\noffice  -50 dBm  wpa2\nguest   -70 dBm  open\n"
    );
    let outcome = run(&bench, &["-o", "json", "action", "radio", "scan"]).await;
    let json = outcome.ok().json();
    assert_eq!(json["records"][1]["ssid"], "guest");
    assert_eq!(json["records"][0]["rssi"], -50);
    assert_eq!(json["units"]["rssi"], "dBm");

    let outcome = run(&bench, &["action", "radio", "reconnect"]).await;
    assert_eq!(outcome.ok().stdout, "Done\n");
    let outcome = run(&bench, &["action", "radio", "tune", "channel=11"]).await;
    outcome.ok();
    assert!(
        bench
            .radio
            .calls
            .borrow()
            .contains(&"channel=11".to_owned())
    );
}

#[tokio::test(flavor = "current_thread")]
async fn action_arguments_are_checked_and_failures_are_typed() {
    let bench = Bench::new();
    let outcome = run(&bench, &["action", "radio", "tune"]).await;
    assert!(
        outcome.message().contains("radio tune needs: channel"),
        "{}",
        outcome.message()
    );
    let outcome = run(&bench, &["action", "radio", "tune", "channel=high"]).await;
    assert!(
        outcome
            .message()
            .contains("channel: expected a non-negative integer"),
        "{}",
        outcome.message()
    );
    let outcome = run(&bench, &["action", "radio", "tune", "speed=1"]).await;
    assert!(
        outcome.message().contains("has no field `speed`"),
        "{}",
        outcome.message()
    );
    let outcome = run(
        &bench,
        &["action", "radio", "tune", "channel=1", "channel=2"],
    )
    .await;
    assert!(
        outcome.message().contains("given twice"),
        "{}",
        outcome.message()
    );
    let outcome = run(&bench, &["action", "radio", "missing"]).await;
    assert!(matches!(
        outcome.device_error(),
        Error::Device(rohi::model::ErrorCode::NotFound)
    ));

    let outcome = run(&bench, &["action", "radio", "explode"]).await;
    assert!(matches!(outcome.device_error(), Error::Rejected(reason) if reason == "not_today"));
    assert!(
        outcome
            .message()
            .contains("the device rejected the action: not_today"),
        "{}",
        outcome.message()
    );
    assert!(
        bench
            .radio
            .calls
            .borrow()
            .iter()
            .all(|call| call != "channel=1")
    );
}

// ---------------------------------------------------------------------------
// identity, through the generic commands
// ---------------------------------------------------------------------------

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[tokio::test(flavor = "current_thread")]
async fn identity_is_provisioned_and_erased_with_the_generic_commands() {
    let bench = Bench::new();
    let outcome = run(&bench, &["status", "identity"]).await;
    assert!(
        outcome.ok().stdout.contains("not_provisioned"),
        "{}",
        outcome.stdout
    );

    let derived = rohi::derive_identity(DEV_PHRASE, 7).unwrap();
    let secret = hex(derived.secret().expose_secret());
    let terminal = Terminal {
        interactive: false,
        input: &[&secret],
        ..Default::default()
    };
    let outcome = run_with(
        &bench,
        &["action", "identity", "provision", "secret=-"],
        terminal,
    )
    .await;
    outcome.ok();
    assert_eq!(
        bench.identity.0.borrow().as_ref().unwrap().public_key(),
        derived.public_key()
    );
    assert!(!outcome.stdout.contains(&secret) && !outcome.stderr.contains(&secret));

    // The device never replaces an identity.
    let terminal = Terminal {
        interactive: false,
        input: &[&secret],
        ..Default::default()
    };
    let outcome = run_with(
        &bench,
        &["action", "identity", "provision", "secret=-"],
        terminal,
    )
    .await;
    assert!(
        matches!(outcome.device_error(), Error::Rejected(reason) if reason == "already_provisioned")
    );
    assert!(
        outcome.message().contains("rohi action run identity erase"),
        "{}",
        outcome.message()
    );

    run(&bench, &["action", "identity", "erase"]).await.ok();
    assert!(bench.identity.0.borrow().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn a_malformed_secret_is_not_echoed() {
    let bench = Bench::new();
    let terminal = Terminal {
        interactive: false,
        input: &["zz-hidden-secret"],
        ..Default::default()
    };
    let outcome = run_with(
        &bench,
        &["action", "identity", "provision", "secret=-"],
        terminal,
    )
    .await;
    assert!(matches!(outcome.error(), CliError::Usage(_)));
    assert!(
        !outcome.message().contains("zz-hidden-secret"),
        "{}",
        outcome.message()
    );
    assert!(bench.identity.0.borrow().is_none());
}

// ---------------------------------------------------------------------------
// factory-reset
// ---------------------------------------------------------------------------

async fn configure_everything(bench: &Bench) {
    run(bench, &["config", "gadget", "label=lamp"]).await.ok();
    run(
        bench,
        &["config", "location", "latitude=1.5", "longitude=2.5"],
    )
    .await
    .ok();
    assert!(bench.gadget.value.borrow().is_some());
    assert!(bench.location.mem.value.borrow().is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn factory_reset_removes_every_section() {
    let bench = Bench::new();
    configure_everything(&bench).await;

    let outcome = run(&bench, &["factory-reset", "--yes"]).await;
    outcome.ok();
    assert!(
        outcome.stdout.starts_with("Factory reset: "),
        "{}",
        outcome.stdout
    );
    assert!(outcome.stdout.contains("rebooting"), "{}", outcome.stdout);
    assert_eq!(common::reboots(), 1);
    assert!(bench.gadget.value.borrow().is_none());
    assert!(bench.location.mem.value.borrow().is_none());

    let json = run(&bench, &["-o", "json", "factory-reset", "--yes"])
        .await
        .ok()
        .json();
    assert_eq!(json["restart_required"], true);
    assert_eq!(json["rebooting"], true);
    assert_eq!(common::reboots(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn factory_reset_asks_only_where_somebody_can_answer() {
    let bench = Bench::new();
    configure_everything(&bench).await;

    let outcome = run(&bench, &["factory-reset"]).await;
    assert!(matches!(outcome.error(), CliError::ConfirmationRequired));
    assert!(bench.gadget.value.borrow().is_some());

    let terminal = Terminal {
        interactive: true,
        input: &["n"],
        ..Default::default()
    };
    let outcome = run_with(&bench, &["factory-reset"], terminal).await;
    assert!(matches!(outcome.error(), CliError::Cancelled));
    assert!(bench.gadget.value.borrow().is_some());

    let terminal = Terminal {
        interactive: true,
        input: &["y"],
        ..Default::default()
    };
    run_with(&bench, &["factory-reset"], terminal).await.ok();
    assert!(bench.gadget.value.borrow().is_none());
    assert!(bench.location.mem.value.borrow().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn reboot_asks_the_device_to_reboot() {
    let bench = Bench::new();
    let outcome = run(&bench, &["reboot"]).await;
    assert_eq!(outcome.ok().stdout, "The device is rebooting\n");
    assert_eq!(common::reboots(), 1);
    let json = run(&bench, &["-o", "json", "reboot"]).await.ok().json();
    assert_eq!(json["rebooting"], true);
    assert_eq!(common::reboots(), 2);
}

// ---------------------------------------------------------------------------
// Output contract
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn human_results_go_to_stdout_only_and_json_is_versioned() {
    let bench = Bench::new();
    for args in [
        &["status"][..],
        &["status", "radio"],
        &["config"],
        &["config", "gadget"],
        &["config", "gadget", "--help"],
        &["action"],
        &["action", "radio"],
        &["action", "radio", "scan", "--help"],
    ] {
        let outcome = run(&bench, args).await;
        outcome.ok();
        assert!(!outcome.stdout.is_empty(), "{args:?}");
        assert!(outcome.stderr.is_empty(), "{args:?}: {}", outcome.stderr);
    }
    for args in [
        &["-o", "json", "status"][..],
        &["-o", "json", "config"],
        &["-o", "json", "config", "gadget"],
        &["-o", "json", "config", "gadget", "--help"],
        &["-o", "json", "action"],
        &["-o", "json", "action", "radio"],
        &["-o", "json", "action", "radio", "scan", "--help"],
    ] {
        let outcome = run(&bench, args).await;
        assert_eq!(outcome.ok().json()["schema_version"], 1, "{args:?}");
    }
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
        let bench = Bench::new();
        let (client, device) = link(244);
        let controller = Controller::new(backend(&bench), OpenPolicy);
        let mut server = ControlBle::new(device, &controller);
        let error = tokio::select! {
            () = async { server.run().await } => unreachable!(),
            result = async {
                let mut ctx = Ctx::new(
                    Io::scripted(Failing(kind), Buf::default(), false, &[]),
                    rohi::cli::OutputFormat::Human,
                );
                let mut device = Device::new(client);
                let command = parse(&["status"]).unwrap().command;
                execute(&mut device, command, &mut ctx).await
            } => result.unwrap_err(),
        };
        assert_eq!(error.is_broken_pipe(), quiet, "{kind:?}");
        assert!(matches!(error, CliError::Output(_)));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_schema_of_many_fields_arrives_complete_in_one_response() {
    // The telemetry section has five fields, all in one schema response.
    let bench = Bench::new();
    run(
        &bench,
        &["config", "connectivity", "host=a.example.org", "port=443"],
    )
    .await
    .ok();
    let outcome = run(&bench, &["config", "connectivity"]).await;
    outcome.ok();
    assert_eq!(
        outcome.stdout,
        "enabled     false\nnode_id     0\ninterval_s  60 s\nhost        a.example.org\nport        443\n"
    );
    let outcome = run(&bench, &["config", "connectivity", "--help"]).await;
    assert!(outcome.ok().stdout.contains("port"), "{}", outcome.stdout);
}

// ---------------------------------------------------------------------------
// location
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn the_location_is_set_with_the_generic_config_and_shows_in_the_status() {
    let bench = Bench::new();
    let outcome = run(&bench, &["status", "location"]).await;
    assert!(
        outcome.ok().stdout.contains("State:    disabled"),
        "{}",
        outcome.stdout
    );
    assert!(outcome.stdout.contains("INFO"), "{}", outcome.stdout);
    assert!(outcome.stdout.contains("not_set"), "{}", outcome.stdout);

    let outcome = run(
        &bench,
        &[
            "config",
            "location",
            "latitude=52.52",
            "longitude=13.405",
            "height_m=34",
        ],
    )
    .await;
    assert_eq!(
        outcome.ok().stdout,
        "latitude   52.52 deg\nlongitude  13.405 deg\nheight_m   34 m\n"
    );

    let outcome = run(&bench, &["config", "location"]).await;
    assert_eq!(
        outcome.ok().stdout,
        "latitude   52.52 deg\nlongitude  13.405 deg\nheight_m   34 m\n"
    );

    let outcome = run(&bench, &["status", "location"]).await;
    outcome.ok();
    assert!(
        outcome.stdout.contains("State:    running"),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("latitude   52.52 deg"),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("source     configured"),
        "{}",
        outcome.stdout
    );
    let outcome = run(&bench, &["-o", "json", "status", "location"]).await;
    let json = outcome.ok().json();
    assert_eq!(json["values"]["longitude"], 13.405);
    assert_eq!(json["values"]["source"], "configured");
    assert_eq!(json["units"]["height_m"], "m");
}

#[tokio::test(flavor = "current_thread")]
async fn a_location_needs_both_coordinates_in_range() {
    let bench = Bench::new();
    // A first height alone must not store "0, 0".
    let outcome = run(&bench, &["config", "location", "height_m=10"]).await;
    assert!(
        matches!(outcome.error(), CliError::Device(_)),
        "{}",
        outcome.message()
    );
    let outcome = run(&bench, &["config", "location", "latitude=52.5"]).await;
    assert!(matches!(outcome.error(), CliError::Device(_)));
    let outcome = run(
        &bench,
        &["config", "location", "latitude=91", "longitude=0"],
    )
    .await;
    assert!(matches!(
        outcome.device_error(),
        Error::Device(rohi::model::ErrorCode::InvalidArgument)
    ));
    let outcome = run(
        &bench,
        &["config", "location", "latitude=north", "longitude=0"],
    )
    .await;
    assert!(
        outcome.message().contains("latitude: expected a number"),
        "{}",
        outcome.message()
    );
    assert!(bench.location.mem.value.borrow().is_none());

    run(
        &bench,
        &["config", "location", "latitude=1.5", "longitude=-2.5"],
    )
    .await
    .ok();
    // The height can be removed again: it follows the default, no height.
    run(&bench, &["config", "location", "height_m=12"])
        .await
        .ok();
    let outcome = run(&bench, &["config", "location", "height_m=null"]).await;
    assert!(
        outcome.ok().stdout.contains("height_m   <unset>\n"),
        "{}",
        outcome.stdout
    );
    assert_eq!(
        bench.location.mem.value.borrow().clone().unwrap().height_m,
        rohi_config::Override::Default
    );
    // Without a coordinate the position is not valid any more.
    let outcome = run(&bench, &["config", "location", "latitude=null"]).await;
    assert!(
        matches!(outcome.error(), CliError::Device(_)),
        "{}",
        outcome.message()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn resetting_the_location_makes_the_device_not_know_where_it_is() {
    let bench = Bench::new();
    run(&bench, &["config", "location", "latitude=1", "longitude=2"])
        .await
        .ok();
    assert!(bench.location.shared.current().is_some());
    run(&bench, &["config", "reset", "location", "--yes"])
        .await
        .ok();
    assert!(bench.location.shared.current().is_none());
    let outcome = run(&bench, &["status", "location"]).await;
    assert!(
        outcome.ok().stdout.contains("State:    disabled"),
        "{}",
        outcome.stdout
    );
}

#[tokio::test(flavor = "current_thread")]
async fn export_and_apply_carry_the_location() {
    let bench = Bench::new();
    run(
        &bench,
        &["config", "location", "latitude=52.52", "longitude=13.405"],
    )
    .await
    .ok();
    let outcome = run(&bench, &["config", "export"]).await;
    assert_eq!(
        outcome.ok().json()["location"],
        json!({ "latitude": 52.52, "longitude": 13.405 })
    );
    let file = temp_file("location.json", r#"{ "location": { "height_m": 40.5 } }"#);
    let outcome = run(&bench, &["config", "apply", path(&file), "--yes"]).await;
    outcome.ok();
    assert_eq!(
        bench.location.mem.value.borrow().clone().unwrap().height_m,
        rohi_config::Override::Value(Some(40.5))
    );
}

// ---------------------------------------------------------------------------
// optimistic concurrency of `config apply`
// ---------------------------------------------------------------------------

/// A client link that lets another client change the device right before its
/// `at`-th request is sent; `at == 0` never does.
struct Interfering {
    inner: ClientEnd,
    sent: usize,
    at: usize,
    seen: Rc<std::cell::Cell<usize>>,
    rival: Option<Device<ClientEnd>>,
}

impl rohi::ClientTransport for Interfering {
    fn max_payload(&self) -> usize {
        self.inner.max_payload()
    }

    async fn send(&mut self, chunk: &[u8]) -> rohi::Result<()> {
        self.sent += 1;
        self.seen.set(self.sent);
        if self.sent == self.at
            && let Some(mut rival) = self.rival.take()
        {
            let port = rohi::model::Mutation {
                field_id: 5,
                op: rohi::model::MutationOp::Set(rohi::model::Value::U64(9000)),
            };
            rival
                .config_patch("connectivity", &[port], None, false)
                .await
                .unwrap();
        }
        self.inner.send(chunk).await
    }

    async fn recv(&mut self) -> rohi::Result<Vec<u8>> {
        self.inner.recv().await
    }
}

/// Applies `file` while a rival client changes the device before request
/// number `at`; returns how many requests were sent.
async fn apply_with_rival(bench: &Bench, file: &std::path::Path, at: usize) -> (Outcome, usize) {
    let cli = parse(&["config", "apply", path(file), "--yes"]).unwrap();
    let (stdout, stderr) = (Buf::default(), Buf::default());
    let mut ctx = Ctx::new(
        Io::scripted(stdout.clone(), stderr.clone(), false, &[]),
        cli.output,
    );
    ctx.uri = Some("ble://BENCH-1234".into());

    let (client, device) = link(244);
    let (rival_client, rival_device) = link(244);
    let controller = Controller::new(backend(bench), OpenPolicy);
    let mut server = ControlBle::new(device, &controller);
    let mut rival_server = ControlBle::new(rival_device, &controller);
    let seen = Rc::new(std::cell::Cell::new(0));
    let transport = Interfering {
        inner: client,
        sent: 0,
        at,
        seen: seen.clone(),
        rival: Some(Device::new(rival_client)),
    };
    let command = cli.command;
    let result = tokio::select! {
        () = async { server.run().await } => unreachable!(),
        () = async { rival_server.run().await } => unreachable!(),
        result = async {
            let mut device = Device::new(transport);
            execute(&mut device, command, &mut ctx).await
        } => result,
    };
    let outcome = Outcome {
        result,
        stdout: stdout.text(),
        stderr: stderr.text(),
    };
    (outcome, seen.get())
}

#[tokio::test(flavor = "current_thread")]
async fn apply_conflicts_when_the_section_changes_after_the_diff_was_shown() {
    let file = temp_file("race.json", r#"{ "connectivity": { "interval_s": 120 } }"#);

    // Undisturbed, the last request is the store of the section.
    let bench = configured().await;
    let (outcome, requests) = apply_with_rival(&bench, &file, 0).await;
    outcome.ok();
    assert_eq!(
        bench.telemetry.effective::<ConnectivityConfig>().interval_s,
        120
    );

    // Another client gets in just before that store: nothing is overwritten.
    let bench = configured().await;
    let (outcome, _) = apply_with_rival(&bench, &file, requests).await;
    let CliError::PartialApply(partial) = outcome.error() else {
        panic!("{}", outcome.message());
    };
    assert_eq!(partial.failed, "connectivity");
    assert!(partial.applied.is_empty());
    assert!(matches!(
        partial.source,
        Error::Device(rohi::model::ErrorCode::Conflict)
    ));
    let stored = bench.telemetry.effective::<ConnectivityConfig>();
    assert_eq!(stored.port, 9000);
    assert_eq!(stored.interval_s, 60);
}

#[tokio::test(flavor = "current_thread")]
async fn none_sets_an_explicit_empty_override_and_null_clears_it() {
    let bench = Bench::new();
    run(&bench, &["config", "gadget", "label=lamp", "mode=slow"])
        .await
        .ok();

    // `none` is an override without a value; the section says so.
    let outcome = run(&bench, &["config", "gadget", "mode=none"]).await;
    assert!(
        outcome.ok().stdout.contains("mode    <none>\n"),
        "{}",
        outcome.stdout
    );
    assert_eq!(
        bench.gadget.value.borrow().clone().unwrap().mode,
        Override::Value(None)
    );
    let outcome = run(&bench, &["-o", "json", "config", "gadget"]).await;
    let json = outcome.ok().json();
    assert_eq!(json["values"]["mode"], Value::Null);
    assert_eq!(json["overridden"], json!(["label", "mode"]));

    // `null` follows the default again, which is no value too, but not an
    // override.
    let outcome = run(&bench, &["config", "gadget", "mode=null"]).await;
    assert!(
        outcome.ok().stdout.contains("mode    <unset>\n"),
        "{}",
        outcome.stdout
    );
    assert_eq!(
        bench.gadget.value.borrow().clone().unwrap().mode,
        Override::Default
    );

    // An open network is an explicit choice, for a secret field as well.
    run(&bench, &["config", "wifi", "ssid=office"]).await.ok();
    let outcome = run(&bench, &["config", "wifi", "password=none"]).await;
    assert!(
        outcome.ok().stdout.contains("password         <none>\n"),
        "{}",
        outcome.stdout
    );
    assert_eq!(
        bench.wifi.value.borrow().clone().unwrap().password,
        Override::Value(None)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn none_is_refused_locally_for_a_field_that_is_not_nullable() {
    let bench = Bench::new();
    let outcome = run(&bench, &["config", "gadget", "level=none"]).await;
    assert!(
        outcome.message().contains("level is not nullable"),
        "{}",
        outcome.message()
    );
    assert_eq!(bench.writes(), 0);
    // Nothing reached the device: this is a usage error.
    assert!(matches!(outcome.error(), CliError::Usage(_)));
}

#[tokio::test(flavor = "current_thread")]
async fn export_diff_and_apply_keep_none_apart_from_clear_and_default() {
    let bench = configured().await;
    run(&bench, &["config", "gadget", "mode=none"]).await.ok();
    let exported = run(&bench, &["config", "export"]).await.ok().json();
    assert_eq!(
        exported["gadget"],
        json!({ "label": "lamp", "mode": { "none": true } })
    );

    // The exported file matches the device.
    let file = temp_file("none.json", &exported.to_string());
    let outcome = run(&bench, &["config", "diff", path(&file)]).await;
    assert_eq!(outcome.ok().stdout, "No differences\n");

    // From none to a value, to the default, and back to none.
    let value = temp_file("value.json", r#"{ "gadget": { "mode": "slow" } }"#);
    assert_eq!(
        run(&bench, &["config", "diff", path(&value)])
            .await
            .ok()
            .stdout,
        "gadget:\n  ~ mode            (none) -> slow\n"
    );
    let default = temp_file("default.json", r#"{ "gadget": { "mode": null } }"#);
    assert_eq!(
        run(&bench, &["config", "diff", path(&default)])
            .await
            .ok()
            .stdout,
        "gadget:\n  - mode            (none) -> (default)\n"
    );
    run(&bench, &["config", "apply", path(&default), "--yes"])
        .await
        .ok();
    assert_eq!(
        bench.gadget.value.borrow().clone().unwrap().mode,
        Override::Default
    );
    let outcome = run(&bench, &["config", "diff", path(&file)]).await;
    assert_eq!(outcome.ok().stdout, "gadget:\n  + mode            (none)\n");
    run(&bench, &["config", "apply", path(&file), "--yes"])
        .await
        .ok();
    assert_eq!(
        bench.gadget.value.borrow().clone().unwrap().mode,
        Override::Value(None)
    );
    // Nothing is left to do now.
    let outcome = run(&bench, &["config", "diff", path(&file)]).await;
    assert_eq!(outcome.ok().stdout, "No differences\n");

    // A required field cannot be none in a file either.
    let bad = temp_file(
        "bad-none.json",
        r#"{ "gadget": { "level": { "none": true } } }"#,
    );
    let outcome = run(&bench, &["config", "diff", path(&bad)]).await;
    assert!(
        outcome.message().contains("gadget.level is not nullable"),
        "{}",
        outcome.message()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn no_ssid_means_wifi_is_disabled_and_ssid_none_disables_it_explicitly() {
    let bench = Bench::new();
    // Disabled by default: no network name.
    let outcome = run(&bench, &["config", "wifi"]).await;
    assert!(
        outcome.ok().stdout.contains("ssid             <unset>\n"),
        "{}",
        outcome.stdout
    );
    run(&bench, &["config", "wifi", "ssid=office"]).await.ok();
    let outcome = run(&bench, &["config", "wifi", "ssid=none"]).await;
    assert!(
        outcome.ok().stdout.contains("ssid             <none>\n"),
        "{}",
        outcome.stdout
    );
    assert_eq!(
        bench.wifi.value.borrow().clone().unwrap().ssid,
        Override::Value(None)
    );
    let outcome = run(&bench, &["config", "wifi", "ssid=null"]).await;
    assert!(
        outcome.ok().stdout.contains("ssid             <unset>\n"),
        "{}",
        outcome.stdout
    );
}
