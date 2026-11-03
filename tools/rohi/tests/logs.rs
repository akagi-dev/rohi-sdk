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
///////////////////////////////////////////////////////////////////////////////
//! `rohi logs`: the log commands against the real `logging` service on an
//! in-memory device: read, follow, download and clear over the generic
//! action and stream planes.

mod common;

use std::cell::{Cell, RefCell};
use std::io::{self, Write};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use clap::Parser;
use common::link;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use rohi::cli::{Cli, CliError, Ctx, Io, execute};
use rohi::{Device, Error};
use rohi_control::{Controller, Device as BackendDevice, DeviceInfo, OpenPolicy};
use rohi_control_ble::ControlBle;
use rohi_logging::segment::SegmentDecoder;
use rohi_logging::storage::{RamMedium, RingStorage};
use rohi_logging::{
    Backends, Compression, LogClock, LogStorageKind, Logging, LoggingConfig, LoggingService,
};
use rohi_service::Statuses;
use rohi_time::UnixTime;
use serde_json::Value;

// ---------------------------------------------------------------------------
// The device
// ---------------------------------------------------------------------------

struct Clock(Cell<u64>);

impl LogClock for &Clock {
    fn monotonic_ms(&self) -> u64 {
        self.0.get()
    }

    fn wall_time(&self) -> Option<UnixTime> {
        // The record number is its time: whole seconds from 2026-01-01.
        Some(UnixTime::from_secs(1_767_225_600 + self.0.get() / 1000))
    }
}

struct Ram(Option<RamMedium<16>>);

impl Backends for Ram {
    type Storage = RingStorage<RamMedium<16>>;

    async fn open(&mut self, kind: LogStorageKind) -> Option<Self::Storage> {
        match kind {
            LogStorageKind::Internal => self.0.take().map(RingStorage::new),
            LogStorageKind::Sd => None,
        }
    }
}

type Runtime = Logging<NoopRawMutex, &'static Clock, Ram, 512, 4096>;
type Service = LoggingService<'static, NoopRawMutex, &'static Clock, Ram, 512, 4096>;

const CONFIG: LoggingConfig = LoggingConfig {
    level: rohi_logging::LogLevel::Debug,
    storage: LogStorageKind::Internal,
    compression: Compression::Lz4,
    retention_bytes: 4096 * 12,
    segment_bytes: 4096,
    flush_interval_ms: 1_000,
};

struct Bench {
    clock: &'static Clock,
    logging: &'static Runtime,
    service: &'static Service,
}

impl Bench {
    fn new() -> Self {
        let clock: &'static Clock = Box::leak(Box::new(Clock(Cell::new(1_000))));
        let logging: &'static Runtime = Box::leak(Box::new(Logging::new(
            clock,
            Ram(Some(RamMedium::new())),
            CONFIG,
        )));
        let service: &'static Service = Box::leak(Box::new(logging.service()));
        Self {
            clock,
            logging,
            service,
        }
    }

    /// Logs `count` records, numbered from `first` in the message.
    fn log(&self, first: u64, count: u64) {
        for n in first..first + count {
            self.clock.0.set(self.clock.0.get() + 10);
            assert!(self.logging.sink().push(
                log::Level::Info,
                "wifi",
                format_args!("event {n}: connected to access point rssi -{}", 40 + n % 30),
            ));
        }
    }
}

fn backend(
    bench: &Bench,
) -> BackendDevice<(), Statuses<NoopRawMutex, (&'static Service,)>, (&'static Service,)> {
    BackendDevice::new(
        DeviceInfo {
            firmware: "fake-1.0.0",
            board: "bench-esp32c3",
        },
        || 90_000,
        (),
        Statuses::new((bench.service,)),
        (bench.service,),
    )
}

// ---------------------------------------------------------------------------
// The command line
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

    /// The records of the text output, as (sequence, message).
    fn lines(&self) -> Vec<(u64, String)> {
        self.stdout
            .lines()
            .map(|line| {
                let record = rohi_logging::segment::text::parse_line(line)
                    .unwrap_or_else(|| panic!("not a record line: {line:?}"));
                (record.sequence, record.message.as_str().to_owned())
            })
            .collect()
    }
}

fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(std::iter::once("rohi").chain(args.iter().copied()))
}

struct Terminal<'a> {
    interactive: bool,
    input: &'a [&'a str],
}

async fn run_with(
    bench: &Bench,
    args: &[&str],
    terminal: Terminal<'_>,
    watch: Option<(usize, Box<dyn std::future::Future<Output = ()> + '_>)>,
) -> Outcome {
    let cli = parse(args).unwrap();
    let (stdout, stderr) = (Buf::default(), Buf::default());
    let io = Io::scripted(
        stdout.clone(),
        stderr.clone(),
        terminal.interactive,
        terminal.input,
    );
    let mut ctx = Ctx::new(io, cli.output);
    ctx.follow_interval = Duration::from_millis(40);
    let (polls, background) = match watch {
        Some((polls, background)) => (Some(polls), Some(background)),
        None => (None, None),
    };
    ctx.watch_limit = polls;

    let (client, device) = link(244);
    let controller = Controller::new(backend(bench), OpenPolicy);
    let mut server = ControlBle::new(device, &controller);
    let command = cli.command;
    let result = tokio::select! {
        () = async { server.run().await } => unreachable!(),
        result = async {
            let mut device = Device::new(client);
            let background = async {
                if let Some(background) = background {
                    Box::into_pin(background).await;
                }
            };
            let (result, ()) = tokio::join!(execute(&mut device, command, &mut ctx), background);
            result
        } => result,
    };
    Outcome {
        result,
        stdout: stdout.text(),
        stderr: stderr.text(),
    }
}

async fn run(bench: &Bench, args: &[&str]) -> Outcome {
    let terminal = Terminal {
        interactive: false,
        input: &[],
    };
    run_with(bench, args, terminal, None).await
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rohi-logs-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// The records of a downloaded file, each file decoded on its own.
fn decode(path: &Path) -> Vec<u64> {
    let codec = if path.extension().is_some_and(|ext| ext == "lz4") {
        Compression::Lz4
    } else {
        Compression::None
    };
    let bytes = std::fs::read(path).unwrap();
    let mut decoder = SegmentDecoder::new(codec);
    let mut sequences = Vec::new();
    decoder
        .push(&bytes, &mut |record| sequences.push(record.sequence))
        .unwrap();
    assert!(decoder.is_complete(), "{path:?} is not a whole file");
    sequences
}

// ---------------------------------------------------------------------------
// Interface
// ---------------------------------------------------------------------------

#[test]
fn the_logs_command_line_parses() {
    use rohi::cli::{Command, LogsAction};
    let lines = |args: &[&str]| match parse(args).unwrap().command {
        Command::Logs {
            action: None,
            lines,
            follow,
        } => (lines, follow),
        _ => panic!("not `logs`"),
    };
    assert_eq!(lines(&["logs"]), (20, false));
    assert_eq!(lines(&["logs", "-n", "100"]), (100, false));
    assert_eq!(lines(&["logs", "-f"]), (20, true));
    assert_eq!(lines(&["logs", "-n", "100", "-f"]), (100, true));
    assert_eq!(lines(&["logs", "--lines", "5", "--follow"]), (5, true));
    assert_eq!(lines(&["logs", "-n", "0"]), (0, false));

    match parse(&["logs", "download"]).unwrap().command {
        Command::Logs {
            action:
                Some(LogsAction::Download {
                    directory,
                    force: false,
                }),
            ..
        } => assert_eq!(directory, Path::new(".")),
        _ => panic!("not `logs download`"),
    }
    match parse(&["logs", "download", "-d", "./logs", "--force"])
        .unwrap()
        .command
    {
        Command::Logs {
            action: Some(LogsAction::Download { directory, force }),
            ..
        } => assert_eq!((directory, force), ("./logs".into(), true)),
        _ => panic!("not `logs download`"),
    }
    assert!(matches!(
        parse(&["logs", "clear"]).unwrap().command,
        Command::Logs {
            action: Some(LogsAction::Clear { yes: false }),
            ..
        }
    ));
    assert!(parse(&["logs", "clear", "--yes"]).is_ok());
    // Options of the listing do not mix with the subcommands.
    assert!(parse(&["logs", "-n", "5", "download"]).is_err());
    assert!(parse(&["logs", "-n", "many"]).is_err());
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn the_default_is_the_latest_twenty_records_oldest_first() {
    let bench = Bench::new();
    bench.log(1, 50);
    let outcome = run(&bench, &["logs"]).await;
    outcome.ok();
    let lines = outcome.lines();
    assert_eq!(lines.len(), 20);
    assert_eq!(
        lines
            .iter()
            .map(|(sequence, _)| *sequence)
            .collect::<Vec<_>>(),
        (31..=50).collect::<Vec<_>>()
    );
    assert!(lines[0].1.starts_with("event 31:"));
    assert_eq!(outcome.stderr, "");
    // The text is the stable line format of the log files.
    assert!(
        outcome
            .stdout
            .lines()
            .next()
            .unwrap()
            .contains("2026-01-01T00:00:")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn n_selects_the_count_also_across_many_pages() {
    let bench = Bench::new();
    bench.log(1, 400);
    for (count, expected) in [(1usize, 1usize), (7, 7), (100, 100), (250, 250)] {
        let outcome = run(&bench, &["logs", "-n", &count.to_string()]).await;
        outcome.ok();
        let lines = outcome.lines();
        assert_eq!(lines.len(), expected, "-n {count}");
        let first = 400 - expected as u64 + 1;
        assert_eq!(lines.first().unwrap().0, first, "-n {count}");
        assert_eq!(lines.last().unwrap().0, 400);
        for pair in lines.windows(2) {
            assert_eq!(pair[1].0, pair[0].0 + 1);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn fewer_records_than_asked_are_all_of_them() {
    let bench = Bench::new();
    bench.log(1, 5);
    let outcome = run(&bench, &["logs", "-n", "100"]).await;
    outcome.ok();
    assert_eq!(outcome.lines().len(), 5);
    assert_eq!(outcome.lines()[0].0, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn an_empty_log_prints_nothing_and_says_so_on_stderr() {
    let bench = Bench::new();
    let outcome = run(&bench, &["logs"]).await;
    outcome.ok();
    assert_eq!(outcome.stdout, "");
    assert!(outcome.stderr.contains("No log records"));
    let outcome = run(&bench, &["-o", "json", "logs"]).await;
    outcome.ok();
    assert_eq!(outcome.json()["records"], serde_json::json!([]));
    assert_eq!(outcome.json()["schema_version"], 1);
}

#[tokio::test(flavor = "current_thread")]
async fn n_zero_prints_no_records() {
    let bench = Bench::new();
    bench.log(1, 10);
    let outcome = run(&bench, &["logs", "-n", "0"]).await;
    outcome.ok();
    assert_eq!(outcome.stdout, "");
}

#[tokio::test(flavor = "current_thread")]
async fn json_has_one_object_per_record() {
    let bench = Bench::new();
    bench.log(1, 5);
    let outcome = run(&bench, &["-o", "json", "logs", "-n", "2"]).await;
    outcome.ok();
    let json = outcome.json();
    assert_eq!(json["schema_version"], 1);
    let records = json["records"].as_array().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["sequence"], 4);
    assert_eq!(records[1]["sequence"], 5);
    assert_eq!(records[1]["level"], "info");
    assert_eq!(records[1]["target"], "wifi");
    assert!(
        records[1]["message"]
            .as_str()
            .unwrap()
            .starts_with("event 5:")
    );
    assert_eq!(records[1]["truncated"], false);
    assert!(records[1]["uptime_ms"].as_u64().unwrap() > 0);
    assert!(
        records[1]["time"]
            .as_str()
            .unwrap()
            .starts_with("2026-01-01T00:00:")
    );
    assert!(records[1]["time"].as_str().unwrap().ends_with('Z'));
}

// ---------------------------------------------------------------------------
// Following
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn follow_prints_the_latest_and_then_only_new_records() {
    let bench = Bench::new();
    bench.log(1, 10);
    let injector = async {
        tokio::time::sleep(Duration::from_millis(60)).await;
        bench.log(11, 3);
        tokio::time::sleep(Duration::from_millis(60)).await;
        bench.log(14, 2);
    };
    let outcome = run_with(
        &bench,
        &["logs", "-n", "2", "-f"],
        Terminal {
            interactive: false,
            input: &[],
        },
        Some((4, Box::new(injector))),
    )
    .await;
    outcome.ok();
    let sequences: Vec<u64> = outcome
        .lines()
        .iter()
        .map(|(sequence, _)| *sequence)
        .collect();
    // The last two, then each new record once and in order.
    assert_eq!(sequences, [9, 10, 11, 12, 13, 14, 15]);
}

#[tokio::test(flavor = "current_thread")]
async fn follow_with_n_zero_starts_at_the_end_and_json_is_one_document_per_line() {
    let bench = Bench::new();
    bench.log(1, 10);
    let injector = async {
        tokio::time::sleep(Duration::from_millis(60)).await;
        bench.log(11, 2);
    };
    let outcome = run_with(
        &bench,
        &["-o", "json", "logs", "-n", "0", "-f"],
        Terminal {
            interactive: false,
            input: &[],
        },
        Some((3, Box::new(injector))),
    )
    .await;
    outcome.ok();
    let documents: Vec<Value> = outcome
        .stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let sequences: Vec<u64> = documents
        .iter()
        .map(|document| document["records"][0]["sequence"].as_u64().unwrap())
        .collect();
    assert_eq!(sequences, [11, 12]);
    assert!(
        documents
            .iter()
            .all(|document| document["schema_version"] == 1)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn follow_on_an_empty_log_waits_for_the_first_record() {
    let bench = Bench::new();
    let injector = async {
        tokio::time::sleep(Duration::from_millis(60)).await;
        bench.log(1, 2);
    };
    let outcome = run_with(
        &bench,
        &["logs", "-f"],
        Terminal {
            interactive: false,
            input: &[],
        },
        Some((3, Box::new(injector))),
    )
    .await;
    outcome.ok();
    assert_eq!(outcome.lines().len(), 2);
}

// ---------------------------------------------------------------------------
// Downloading
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn download_writes_every_retained_file_unchanged_to_the_directory() {
    let bench = Bench::new();
    bench.log(1, 400);
    let dir = temp_dir("download");
    let outcome = run(&bench, &["logs", "download", "-d", dir.to_str().unwrap()]).await;
    outcome.ok();

    let names = files_in(&dir);
    assert!(names.len() >= 2, "{names:?}");
    for (index, name) in names.iter().enumerate() {
        assert_eq!(name, &format!("rohi{}.log.lz4", index + 1).as_str());
    }
    // Each file decodes on its own; together they are the retained history.
    let mut sequences = Vec::new();
    for name in &names {
        let own = decode(&dir.join(name));
        assert!(!own.is_empty());
        sequences.extend(own);
    }
    for pair in sequences.windows(2) {
        assert_eq!(pair[1], pair[0] + 1, "a gap between the files");
    }
    assert_eq!(*sequences.last().unwrap(), 400);
    assert!(outcome.stdout.contains(&format!("{} file(s)", names.len())));
    assert!(outcome.stderr.contains("rohi1.log.lz4"));
    // Nothing partial is left behind.
    assert!(names.iter().all(|name| !name.ends_with(".part")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "current_thread")]
async fn download_json_lists_the_files() {
    let bench = Bench::new();
    bench.log(1, 120);
    let dir = temp_dir("json");
    let outcome = run(
        &bench,
        &[
            "-o",
            "json",
            "logs",
            "download",
            "-d",
            dir.to_str().unwrap(),
        ],
    )
    .await;
    outcome.ok();
    let json = outcome.json();
    let files = json["files"].as_array().unwrap();
    assert!(!files.is_empty());
    for file in files {
        let path = file["path"].as_str().unwrap();
        assert_eq!(
            std::fs::metadata(path).unwrap().len(),
            file["size_bytes"].as_u64().unwrap()
        );
        assert!(file["name"].as_str().unwrap().starts_with("rohi"));
    }
    assert_eq!(json["directory"], dir.to_str().unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "current_thread")]
async fn a_download_does_not_replace_files_unless_forced_and_always_closes_its_stream() {
    let bench = Bench::new();
    bench.log(1, 150);
    let dir = temp_dir("force");
    let args = ["logs", "download", "-d", dir.to_str().unwrap()];

    run(&bench, &args).await.ok();
    let first = std::fs::read(dir.join("rohi1.log.lz4")).unwrap();

    // The files are there: refused before anything is written.
    let again = run(&bench, &args).await;
    match again.result {
        Err(CliError::Usage(ref message)) => {
            assert!(message.contains("rohi1.log.lz4"), "{message}");
            assert!(message.contains("--force"));
        }
        ref other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read(dir.join("rohi1.log.lz4")).unwrap(), first);

    // Forced, three times over: the device allows two open streams, so a
    // stream that was not closed would make this fail.
    for _ in 0..3 {
        run(
            &bench,
            &["logs", "download", "-d", dir.to_str().unwrap(), "--force"],
        )
        .await
        .ok();
    }
    assert!(files_in(&dir).iter().all(|name| !name.ends_with(".part")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "current_thread")]
async fn downloading_an_empty_log_creates_nothing() {
    let bench = Bench::new();
    let dir = temp_dir("empty");
    let outcome = run(&bench, &["logs", "download", "-d", dir.to_str().unwrap()]).await;
    outcome.ok();
    assert!(outcome.stdout.contains("no log files"));
    assert!(!dir.exists());
}

#[tokio::test(flavor = "current_thread")]
async fn records_logged_during_and_after_a_download_are_not_in_it() {
    let bench = Bench::new();
    bench.log(1, 100);
    let dir = temp_dir("snapshot");
    run(&bench, &["logs", "download", "-d", dir.to_str().unwrap()])
        .await
        .ok();
    let downloaded: usize = files_in(&dir)
        .iter()
        .map(|name| decode(&dir.join(name)).len())
        .sum();
    assert_eq!(downloaded, 100);
    // They are still there to read, in a new segment.
    bench.log(101, 5);
    let outcome = run(&bench, &["logs", "-n", "5"]).await;
    outcome.ok();
    assert_eq!(outcome.lines().first().unwrap().0, 101);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Clearing
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn clear_asks_first_and_is_refused_when_it_cannot_ask() {
    let bench = Bench::new();
    bench.log(1, 30);
    let outcome = run(&bench, &["logs", "clear"]).await;
    assert!(matches!(
        outcome.result,
        Err(CliError::ConfirmationRequired)
    ));
    assert_eq!(run(&bench, &["logs"]).await.lines().len(), 20);

    // Declined.
    let declined = run_with(
        &bench,
        &["logs", "clear"],
        Terminal {
            interactive: true,
            input: &["n"],
        },
        None,
    )
    .await;
    assert!(matches!(declined.result, Err(CliError::Cancelled)));
    assert_eq!(run(&bench, &["logs", "-n", "100"]).await.lines().len(), 30);

    // Confirmed.
    let confirmed = run_with(
        &bench,
        &["logs", "clear"],
        Terminal {
            interactive: true,
            input: &["y"],
        },
        None,
    )
    .await;
    confirmed.ok();
    assert!(confirmed.stdout.contains("cleared"));
    assert!(confirmed.stderr.contains("Continue?"));
    assert_eq!(run(&bench, &["logs", "-n", "100"]).await.stdout, "");
}

#[tokio::test(flavor = "current_thread")]
async fn clear_yes_does_not_ask_and_logging_goes_on() {
    let bench = Bench::new();
    bench.log(1, 30);
    // Committed on the device before it is cleared.
    run(&bench, &["logs"]).await.ok();
    let outcome = run(&bench, &["logs", "clear", "--yes"]).await;
    outcome.ok();
    assert_eq!(outcome.stderr, "");
    let json = run(&bench, &["-o", "json", "logs", "clear", "-y"]).await;
    json.ok();
    assert_eq!(json.json()["cleared"], true);

    bench.log(31, 3);
    let after = run(&bench, &["logs", "-n", "10"]).await;
    after.ok();
    // The sequence goes on; it never restarts at 1.
    assert_eq!(
        after.lines().iter().map(|(s, _)| *s).collect::<Vec<_>>(),
        [31, 32, 33]
    );
}

// ---------------------------------------------------------------------------
// The generic interface is still there
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn the_actions_are_ordinary_actions_of_the_logging_service() {
    let bench = Bench::new();
    bench.log(1, 3);
    let outcome = run(&bench, &["-o", "json", "action"]).await;
    outcome.ok();
    let text = outcome.stdout;
    for name in ["read", "download", "clear"] {
        assert!(text.contains(&format!("\"{name}\"")), "{text}");
    }
    assert!(text.contains("\"logging\""));

    // `rohi action` cannot show a stream and says so instead of guessing.
    let read = run(&bench, &["action", "logging", "read", "after=0", "limit=2"]).await;
    match read.result {
        Err(CliError::Device(Error::Protocol(ref message))) => {
            assert!(message.contains("stream"), "{message}");
        }
        ref other => panic!("{other:?}"),
    }
    // And it did not leave the stream open: the logs command still works twice.
    run(&bench, &["logs"]).await.ok();
    run(&bench, &["logs"]).await.ok();
}

#[tokio::test(flavor = "current_thread")]
async fn the_logging_service_reports_the_space_of_its_storage() {
    let bench = Bench::new();
    bench.log(1, 40);
    run(&bench, &["logs"]).await.ok();
    let outcome = run(&bench, &["-o", "json", "status", "logging"]).await;
    outcome.ok();
    let text = outcome.stdout;
    for field in [
        "capacity_bytes",
        "used_bytes",
        "free_bytes",
        "dropped_records",
    ] {
        assert!(text.contains(field), "{text}");
    }
}
