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
use rohi_sdk::logging::{Compression, status_field};
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(20);

async fn stream_of(device: &mut Device<TcpLink>, name: &str, args: &[FieldValue]) -> Stream {
    match device.invoke_outcome("logging", name, args).await.unwrap() {
        ActionOutcome::Stream(stream) => stream,
        other => panic!("{name} returned {other:?}"),
    }
}

async fn read_file(device: &mut Device<TcpLink>, stream: &Stream, file: u32) -> Vec<u8> {
    read_stream_file(device, stream, file).await
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
async fn logging_is_a_service_with_download_and_clear_and_the_tail_is_no_action() {
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
    assert_eq!(names, ["clear", "download"]);

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
        let text = archive_text(&mut device).await;
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
        let text = archive_text(&mut device).await;
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
        tail_text(&mut device)
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
        #[cfg(feature = "lz4")]
        let codec = if file.name.ends_with(".lz4") {
            Compression::Lz4
        } else {
            Compression::None
        };
        #[cfg(not(feature = "lz4"))]
        let codec = Compression::None;
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
        let (_, issues) = summary(&mut device, "logging").await;
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
    // The archive has the record once it was committed, which a download does.
    eventually("the boot record", WAIT, async || {
        archive_text(&mut device)
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
        let text = archive_text(&mut device).await;
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

async fn boot_count(device: &mut Device<TcpLink>) -> u64 {
    match fields(device, "system").await.get("boot_count") {
        Some(Value::U64(count)) => *count,
        other => panic!("boot_count is {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn tail_serves_the_records_in_ram_and_follows_them_once() {
    let sim = Sim::start(&Launch::new());
    let mut device = sim.device().await;
    let first = eventually("the boot record", WAIT, async || {
        let tail = tail(&mut device, 0, 0).await;
        tail.text.contains("Starting").then_some(tail)
    })
    .await;
    assert!(!first.reset);
    assert_ne!(first.session, 0);
    let mut cursor = first.last_sequence();
    assert!(cursor > 0);
    // Whole lines of the log format, oldest first, by tail sequence.
    let sequences: Vec<u64> = first
        .text
        .lines()
        .map(|line| line.split(' ').next().unwrap().parse().unwrap())
        .collect();
    assert!(sequences.windows(2).all(|pair| pair[0] + 1 == pair[1]));

    // New records arrive once and in order; nothing new is an empty answer.
    let mut seen = sequences.len();
    for round in 0..3 {
        device
            .config_patch(
                "wireless",
                &[set(wireless::WIFI_SSID, text(&format!("tail-{round}")))],
                false,
            )
            .await
            .unwrap();
        let news = eventually("a new record", WAIT, async || {
            let news = tail(&mut device, first.session, cursor).await;
            (!news.text.is_empty()).then_some(news)
        })
        .await;
        assert!(!news.reset);
        assert!(news.last_sequence() > cursor);
        assert!(news.text.lines().count() >= 1);
        for line in news.text.lines() {
            let sequence: u64 = line.split(' ').next().unwrap().parse().unwrap();
            assert!(sequence > cursor, "{line}");
        }
        cursor = news.last_sequence();
        seen += news.text.lines().count();
    }
    assert!(seen > sequences.len());

    // Another boot's session means start over, with a fresh snapshot.
    let reset = tail(&mut device, first.session.wrapping_add(1), cursor).await;
    assert!(reset.reset);
    assert_eq!(reset.session, first.session);
    assert!(!reset.text.is_empty());
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn tail_answers_while_a_download_is_open_and_does_not_end_it() {
    let sim = Sim::start(&Launch::new());
    let mut device = sim.device().await;
    eventually("a record", WAIT, async || {
        tail_text(&mut device)
            .await
            .contains("Starting")
            .then_some(())
    })
    .await;
    let download = stream_of(&mut device, "download", &[]).await;
    for _ in 0..3 {
        assert!(tail_text(&mut device).await.contains("Starting"));
    }
    // The download is intact after the tails.
    for file in &download.files {
        let data = read_file(&mut device, &download, file.id).await;
        assert_eq!(data.len() as u64, file.size_bytes);
    }
    device.close_stream(&download).await.unwrap();
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn every_start_has_a_session_of_its_own_whatever_the_boot_counter_does() {
    // A file keeps the counter: it grows, and the sessions still all differ.
    let dir = TempDir::new().unwrap();
    let storage = dir.path().join("device.flash");
    let mut sessions = Vec::new();
    for start in 1..=3u64 {
        let sim = Sim::start(&Launch::new().storage(&storage));
        let mut device = sim.device().await;
        assert_eq!(boot_count(&mut device).await, start);
        let tail = eventually("the boot record", WAIT, async || {
            let tail = tail(&mut device, 0, 0).await;
            tail.text.contains("Starting").then_some(tail)
        })
        .await;
        sessions.push(tail.session);
        drop(device);
        assert!(sim.stop().success());
    }
    assert_distinct(&sessions);

    // Nothing keeps the counter (an ephemeral device forgets it, like a storage
    // that cannot be written): it is 1 at every start, and the sessions differ
    // all the same, so a client with the session of an earlier start is told to
    // start over instead of taking the new records for the old ones.
    let mut sessions = Vec::new();
    for _ in 0..3 {
        let sim = Sim::start(&Launch::new());
        let mut device = sim.device().await;
        assert_eq!(boot_count(&mut device).await, 1);
        let tail = eventually("the boot record", WAIT, async || {
            let tail = tail(&mut device, 0, 0).await;
            tail.text.contains("Starting").then_some(tail)
        })
        .await;
        if let Some(earlier) = sessions.last() {
            let stale = tail_page(&mut device, *earlier, 3).await;
            assert!(stale.reset, "the session of the earlier start is not valid");
            assert_eq!(stale.session, tail.session);
        }
        sessions.push(tail.session);
        drop(device);
        assert!(sim.stop().success());
    }
    assert_distinct(&sessions);
}

fn assert_distinct(sessions: &[u32]) {
    let mut sorted = sessions.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), sessions.len(), "{sessions:?}");
    assert!(sessions.iter().all(|session| *session != 0));
}

#[tokio::test(flavor = "current_thread")]
async fn following_the_tail_at_debug_generates_no_records_of_its_own() {
    let sim = Sim::start(&Launch::new());
    let mut device = sim.device().await;
    // The persistent level is the one that feeds the tail: raise it to debug.
    device
        .config_patch("logging", &[set(1, Value::Enum("debug".to_owned()))], false)
        .await
        .unwrap();
    // Roll the bounded tail past the boot record before starting to follow it.
    for round in 0..altruist_urban::logging::TAIL_RECORDS {
        device
            .config_patch(
                "wireless",
                &[set(
                    wireless::WIFI_SSID,
                    text(&format!("debug-tail-{round}")),
                )],
                false,
            )
            .await
            .unwrap();
    }
    let first = eventually("a tail record", WAIT, async || {
        let tail = tail(&mut device, 0, 0).await;
        (!tail.text.is_empty()).then_some(tail)
    })
    .await;
    assert!(!first.text.contains("Starting"));
    // The configuration change itself logs; let it settle, then follow.
    let mut cursor = first.cursor;
    let mut session = first.session;
    let mut settled = 0;
    while settled < 3 {
        let page = tail_page(&mut device, session, cursor).await;
        session = page.session;
        if page.text.is_empty() {
            settled += 1;
        } else {
            settled = 0;
            cursor = page.cursor;
        }
    }
    // Polling for a while now creates nothing to read: every page is empty
    // and the newest sequence of the device does not move.
    let newest = tail_page(&mut device, session, cursor).await.newest;
    for _ in 0..40 {
        let page = tail_page(&mut device, session, cursor).await;
        assert!(page.text.is_empty(), "polling wrote: {:?}", page.text);
        assert_eq!((page.newest, page.cursor, page.gap), (newest, cursor, 0));
        assert!(!page.reset);
    }
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn clear_is_busy_while_a_download_is_open_and_the_tail_is_not_affected() {
    use rohi::{Error, model::ErrorCode};
    let sim = Sim::start(&Launch::new());
    let mut device = sim.device().await;
    eventually("a record in the archive", WAIT, async || {
        archive_text(&mut device)
            .await
            .contains("Starting")
            .then_some(())
    })
    .await;
    let before = tail(&mut device, 0, 0).await;
    let stored = status_number(&mut device, status_field::STORED_RECORDS)
        .await
        .unwrap();
    assert!(stored > 0);

    let stream = stream_of(&mut device, "download", &[]).await;
    for _ in 0..2 {
        match device.invoke("logging", "clear", &[]).await {
            Err(Error::Device(ErrorCode::Busy)) => {}
            other => panic!("clear while a download is open: {other:?}"),
        }
    }
    assert_eq!(
        status_number(&mut device, status_field::STORED_RECORDS).await,
        Some(stored),
        "nothing was cleared"
    );
    // The download is intact and the tail answers as before.
    for file in &stream.files {
        let data = read_file(&mut device, &stream, file.id).await;
        assert_eq!(data.len() as u64, file.size_bytes);
    }
    let during = tail(&mut device, 0, 0).await;
    assert_eq!(during.session, before.session);
    assert!(during.text.starts_with(before.text.lines().next().unwrap()));

    device.close_stream(&stream).await.unwrap();
    device.invoke("logging", "clear", &[]).await.unwrap();
    eventually("an empty history", WAIT, async || {
        (status_number(&mut device, status_field::STORED_RECORDS).await == Some(0)).then_some(())
    })
    .await;
    // The archive is gone, the RAM tail and its session are not.
    let after = tail(&mut device, 0, 0).await;
    assert_eq!(after.session, before.session);
    assert!(after.text.starts_with(before.text.lines().next().unwrap()));
    let continued = tail_page(&mut device, before.session, before.last_sequence()).await;
    assert!(!continued.reset);
    drop(device);
    sim.stop();
}
