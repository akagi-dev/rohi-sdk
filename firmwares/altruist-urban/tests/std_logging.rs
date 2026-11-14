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

//! The persistent `logging` service on the host: a real process, its log flash
//! in a file, read back through the real `rohi` client over the generic stream
//! plane.

mod common;

use std::time::Duration;

use common::*;
use rohi::model::{ActionOutcome, FieldValue, Stream, Value};
use rohi::{Device, TcpLink};
use rohi_sdk::logging::segment::SegmentDecoder;
use rohi_sdk::logging::{Compression, read_arg, status_field};
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(20);

fn arg(field_id: u32, value: u64) -> FieldValue {
    FieldValue {
        field_id,
        value: Value::U64(value),
    }
}

async fn stream_of(device: &mut Device<TcpLink>, name: &str, args: &[FieldValue]) -> Stream {
    match device.invoke_outcome("logging", name, args).await.unwrap() {
        ActionOutcome::Stream(stream) => stream,
        other => panic!("{name} returned {other:?}"),
    }
}

async fn read_file(device: &mut Device<TcpLink>, stream: &Stream, file: u32) -> Vec<u8> {
    let mut data = Vec::new();
    loop {
        let chunk = device
            .read_stream(stream, file, data.len() as u64)
            .await
            .unwrap();
        data.extend_from_slice(&chunk.data);
        if chunk.eof {
            return data;
        }
        assert!(!chunk.data.is_empty(), "an empty chunk before the end");
    }
}

/// The text `logging.read` returns for every retained record: one call returns
/// about one `READ_BYTES` of text, so it asks again after the last sequence.
async fn records(device: &mut Device<TcpLink>) -> String {
    let mut text = String::new();
    let mut after = 0;
    loop {
        let stream = stream_of(
            device,
            "read",
            &[arg(read_arg::AFTER, after), arg(read_arg::LIMIT, 200)],
        )
        .await;
        let data = read_file(device, &stream, stream.files[0].id).await;
        device.close_stream(&stream).await.unwrap();
        let page = String::from_utf8(data).unwrap();
        let Some(last) = page
            .lines()
            .filter_map(|line| line.split(' ').next()?.parse::<u64>().ok())
            .next_back()
        else {
            return text;
        };
        after = last;
        text.push_str(&page);
    }
}

async fn status_number(device: &mut Device<TcpLink>, field: u32) -> Option<u64> {
    let status = device.service_status("logging").await.unwrap();
    status
        .fields
        .into_iter()
        .find(|entry| entry.field_id == field)
        .and_then(|entry| match entry.value {
            Value::U64(number) => Some(number),
            _ => None,
        })
}

#[tokio::test(flavor = "current_thread")]
async fn logging_is_a_service_with_read_download_and_clear() {
    let sim = Sim::start(&Launch::new());
    let mut device = sim.device().await;

    let services = device.services().await.unwrap();
    assert!(services.iter().any(|service| service.name == "logging"));
    let actions = device.actions().await.unwrap();
    let mut names: Vec<_> = actions
        .iter()
        .filter(|action| action.service == "logging")
        .map(|action| action.name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["clear", "download", "read"]);

    let status = fields(&mut device, "logging").await;
    assert_eq!(status["storage"], text("internal"));
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn the_device_logs_what_it_does_and_the_history_survives_a_restart() {
    let dir = TempDir::new().unwrap();
    let storage = dir.path().join("device.flash");

    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    let first = eventually("the boot record", WAIT, async || {
        let text = records(&mut device).await;
        text.contains("Starting").then_some(text)
    })
    .await;
    let newest = status_number(&mut device, status_field::NEWEST_SEQUENCE)
        .await
        .expect("a newest sequence");
    drop(device);
    assert!(sim.stop().success());
    assert!(
        dir.path().join("device.log.flash").exists(),
        "the log flash is a file next to the configuration flash"
    );

    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    let second = eventually("the records of both runs", WAIT, async || {
        let text = records(&mut device).await;
        (text.matches("Starting").count() >= 2).then_some(text)
    })
    .await;
    for line in first.lines() {
        assert!(second.contains(line), "lost after the restart: {line}");
    }
    let oldest = status_number(&mut device, status_field::OLDEST_SEQUENCE)
        .await
        .unwrap();
    assert_eq!(oldest, 1, "the history was kept, not restarted");
    assert!(
        status_number(&mut device, status_field::NEWEST_SEQUENCE)
            .await
            .unwrap()
            > newest,
        "the sequence continues"
    );
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn download_gives_decodable_files_and_clear_empties_the_log() {
    let sim = Sim::start(&Launch::new());
    let mut device = sim.device().await;
    eventually("a record", WAIT, async || {
        records(&mut device)
            .await
            .contains("Starting")
            .then_some(())
    })
    .await;

    let stream = stream_of(&mut device, "download", &[]).await;
    assert!(!stream.files.is_empty());
    let mut sequences = Vec::new();
    for file in &stream.files {
        assert!(file.name.starts_with("rohi"), "{}", file.name);
        let codec = if file.name.ends_with(".lz4") {
            Compression::Lz4
        } else {
            Compression::None
        };
        let data = read_file(&mut device, &stream, file.id).await;
        assert_eq!(data.len() as u64, file.size_bytes);
        let mut decoder = SegmentDecoder::new(codec);
        decoder
            .push(&data, &mut |record| sequences.push(record.sequence))
            .unwrap();
        assert!(decoder.is_complete(), "{} is not a whole file", file.name);
    }
    device.close_stream(&stream).await.unwrap();
    assert!(!sequences.is_empty());

    let records = device.invoke("logging", "clear", &[]).await.unwrap();
    assert!(records.is_empty());
    eventually("an empty history", WAIT, async || {
        (status_number(&mut device, status_field::STORED_RECORDS).await == Some(0)).then_some(())
    })
    .await;
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn the_sd_card_is_unavailable_on_the_host_and_never_replaced_by_the_file() {
    let sim = Sim::start(&Launch::new());
    let mut device = sim.device().await;
    device
        .config_patch("logging", &[set(2, Value::Enum("sd".to_owned()))], false)
        .await
        .unwrap();
    let issues = eventually("storage_unavailable", WAIT, async || {
        let (_, _, _, issues) = summary(&mut device, "logging").await;
        issues
            .contains(&"storage_unavailable".to_owned())
            .then_some(issues)
    })
    .await;
    assert!(issues.contains(&"storage_unavailable".to_owned()));
    assert_eq!(
        status_number(&mut device, status_field::STORED_RECORDS).await,
        Some(0),
        "no record went to the other backend"
    );
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn a_cleared_log_stays_cleared_across_a_restart_and_keeps_working() {
    let dir = TempDir::new().unwrap();
    let storage = dir.path().join("device.flash");
    let log_file = dir.path().join("device.log.flash");

    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    eventually("the boot record", WAIT, async || {
        records(&mut device)
            .await
            .contains("Starting")
            .then_some(())
    })
    .await;
    let history = status_number(&mut device, status_field::HISTORY)
        .await
        .unwrap();
    device.invoke("logging", "clear", &[]).await.unwrap();
    assert_eq!(
        status_number(&mut device, status_field::HISTORY).await,
        Some(history + 1)
    );
    drop(device);
    assert!(sim.stop().success());
    assert_eq!(
        std::fs::metadata(&log_file).unwrap().len(),
        1 << 20,
        "the file is the 1 MiB region"
    );

    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    let text = eventually("the records of the new boot", WAIT, async || {
        let text = records(&mut device).await;
        text.contains("Starting").then_some(text)
    })
    .await;
    assert_eq!(text.matches("Starting").count(), 1, "{text}");
    assert_eq!(
        status_number(&mut device, status_field::HISTORY).await,
        Some(history + 1),
        "the generation survives the restart, no format"
    );
    drop(device);
    sim.stop();
}

#[test]
fn a_log_image_of_the_former_256_kib_layout_is_a_startup_error() {
    let dir = TempDir::new().unwrap();
    let storage = dir.path().join("device.flash");
    let log_file = dir.path().join("device.log.flash");
    std::fs::write(&log_file, vec![0xFF; 256 * 1024]).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_altruist-urban"))
        .arg("--storage")
        .arg(&storage)
        .args(["--control-port", "0", "--manage-port", "0"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(74));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("256 KiB") && stderr.contains("delete"),
        "{stderr}"
    );
    assert_eq!(std::fs::metadata(&log_file).unwrap().len(), 256 * 1024);
}
