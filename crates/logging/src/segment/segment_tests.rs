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

use std::io::Read;
use std::vec::Vec;

use rohi_time::UnixTime;

use super::*;
use crate::{Compression, LogLevel, LogRecord};

fn record(sequence: u64, message: &str) -> LogRecord {
    let mut record = LogRecord::new(
        LogLevel::Info,
        "wifi",
        format_args!("{message}"),
        sequence * 10,
        sequence
            .is_multiple_of(2)
            .then(|| UnixTime::from_millis(1_800_000_000_000 + sequence)),
    );
    record.sequence = sequence;
    record
}

fn records(from: u64, count: u64) -> Vec<LogRecord> {
    (from..from + count)
        .map(|n| {
            record(
                n,
                &std::format!("connected to access point {n} rssi -{}", 40 + n % 30),
            )
        })
        .collect()
}

/// Batches of the records, like the logging task writes them.
fn batches(records: &[LogRecord]) -> Vec<Batch> {
    let mut all = Vec::new();
    let mut batch = Batch::new();
    for record in records {
        if batch.push(record).is_err() {
            all.push(core::mem::take(&mut batch));
            batch.push(record).unwrap();
        }
    }
    if !batch.is_empty() {
        all.push(batch);
    }
    all
}

/// The payload of a sealed segment.
fn payload(batches: &[Batch], codec: Compression) -> Vec<u8> {
    let mut payload = payload_prefix(codec).to_vec();
    for batch in batches {
        let mut chunk = [0u8; MAX_CHUNK_LEN];
        let len = batch.encode(codec, &mut chunk).unwrap();
        payload.extend_from_slice(&chunk[..len]);
    }
    payload.extend_from_slice(payload_suffix(codec));
    payload
}

fn decode(
    codec: Compression,
    payload: &[u8],
    piece: usize,
) -> (Vec<LogRecord>, Result<(), DecodeError>, SegmentDecoder) {
    let mut decoder = SegmentDecoder::new(codec);
    let mut out = Vec::new();
    let mut result = Ok(());
    for part in payload.chunks(piece) {
        result = decoder.push(part, &mut |record| out.push(record));
        if result.is_err() {
            break;
        }
    }
    (out, result, decoder)
}

#[test]
fn crc32_matches_the_check_value() {
    assert_eq!(Crc32::of(b"123456789"), 0xCBF4_3926);
    let mut crc = Crc32::new();
    crc.update(b"1234");
    crc.update(b"56789");
    assert_eq!(crc.finish(), 0xCBF4_3926);
    assert_eq!(Crc32::of(b""), 0);
}

#[test]
fn a_header_round_trips_and_detects_damage() {
    let header = SegmentHeader {
        codec: Compression::Lz4,
        first_sequence: 17,
        last_sequence: u64::MAX - 1,
        records: 1_234,
        payload_len: 8_000,
        raw_len: 30_000,
        payload_crc: 0xDEAD_BEEF,
    };
    let bytes = header.encode();
    assert_eq!(bytes.len(), HEADER_LEN);
    assert_eq!(&bytes[..5], b"RLG1\x01");
    assert_eq!(SegmentHeader::decode(&bytes), Ok(header));

    for bit in 0..HEADER_LEN * 8 {
        let mut damaged = bytes;
        damaged[bit / 8] ^= 1 << (bit % 8);
        assert_ne!(SegmentHeader::decode(&damaged), Ok(header), "bit {bit}");
    }
    assert_eq!(
        SegmentHeader::decode(&bytes[..43]),
        Err(HeaderError::Truncated)
    );
    assert_eq!(
        SegmentHeader::decode(&[0xFF; HEADER_LEN]),
        Err(HeaderError::Magic)
    );
    assert_eq!(SegmentHeader::decode(&[]), Err(HeaderError::Truncated));
    let mut future = bytes;
    future[4] = 2;
    assert_eq!(SegmentHeader::decode(&future), Err(HeaderError::Version));
}

#[test]
fn files_are_named_by_position_and_codec() {
    assert_eq!(file_name(1, Compression::Lz4).as_str(), "rohi1.log.lz4");
    assert_eq!(file_name(12, Compression::None).as_str(), "rohi12.log");
    assert_eq!(
        file_name(u32::MAX, Compression::Lz4).len(),
        header::MAX_FILE_NAME_LEN
    );
}

#[test]
fn a_batch_is_bounded_and_refuses_what_does_not_fit() {
    let all = records(1, 200);
    let mut batch = Batch::new();
    let mut taken = 0;
    for record in &all {
        if batch.push(record).is_err() {
            break;
        }
        taken += 1;
    }
    assert!(taken > 10 && taken < 200);
    assert_eq!(batch.records(), taken);
    assert!(batch.len() <= MAX_BATCH_BYTES);
    assert_eq!(
        (batch.first_sequence(), batch.last_sequence()),
        (1, taken as u64)
    );
    // A refused record leaves the batch as it was.
    let before = batch.len();
    assert_eq!(batch.push(&all[taken as usize]), Err(BatchFull));
    assert_eq!(batch.len(), before);
    batch.clear();
    assert!(batch.is_empty());
}

#[test]
fn both_codecs_read_back_what_was_written() {
    let written = records(100, 300);
    let batches = batches(&written);
    assert!(batches.len() > 3);
    for codec in [Compression::None, Compression::Lz4] {
        let payload = payload(&batches, codec);
        // Whole, byte by byte and in odd pieces: the same records.
        for piece in [payload.len(), 1, 7, 100, 4_096] {
            let (read, result, decoder) = decode(codec, &payload, piece);
            assert_eq!(result, Ok(()), "{codec:?} {piece}");
            assert_eq!(read, written, "{codec:?} {piece}");
            assert!(decoder.is_complete(), "{codec:?} {piece}");
            assert_eq!(decoder.summary().records, 300);
            assert_eq!(decoder.summary().skipped, 0);
        }
    }
}

#[test]
fn compression_shrinks_a_log() {
    let batches = batches(&records(1, 300));
    let plain = payload(&batches, Compression::None).len();
    let packed = payload(&batches, Compression::Lz4).len();
    assert!(packed * 2 < plain, "{packed} vs {plain}");
}

#[test]
fn a_downloaded_lz4_segment_is_a_standard_text_file() {
    let written = records(1, 150);
    let file = payload(&batches(&written), Compression::Lz4);
    let mut text = std::string::String::new();
    lz4_flex::frame::FrameDecoder::new(file.as_slice())
        .read_to_string(&mut text)
        .unwrap();
    let lines: Vec<_> = text
        .lines()
        .map(|line| text::parse_line(line).unwrap())
        .collect();
    assert_eq!(lines, written);
}

#[test]
fn a_segment_cut_anywhere_yields_a_prefix_and_never_panics() {
    let written = records(1, 120);
    let batches = batches(&written);
    for codec in [Compression::None, Compression::Lz4] {
        let payload = payload(&batches, codec);
        let mut last = 0;
        for cut in 0..=payload.len() {
            let (read, _, decoder) = decode(codec, &payload[..cut], 13);
            assert_eq!(read, written[..read.len()], "{codec:?} cut {cut}");
            assert!(read.len() >= last, "monotonic");
            last = read.len();
            assert_eq!(
                decoder.is_complete(),
                cut == payload.len()
                    || (codec == Compression::None
                        && (cut == 0 || payload[..cut].ends_with(b"\n")))
            );
        }
        assert_eq!(last, written.len());
    }
}

#[test]
fn a_damaged_block_stops_the_segment_at_the_block_before() {
    let written = records(1, 200);
    let batches = batches(&written);
    let payload = payload(&batches, Compression::Lz4);
    let first = batches[0].records() as usize;

    // Flip one bit inside the second block.
    let mut first_chunk = [0u8; MAX_CHUNK_LEN];
    let first_len = batches[0]
        .encode(Compression::Lz4, &mut first_chunk)
        .unwrap();
    let mut damaged = payload.clone();
    damaged[lz4::FRAME_HEADER_LEN + first_len + 6] ^= 0x04;
    let (read, result, decoder) = decode(Compression::Lz4, &damaged, 64);
    assert_eq!(result, Err(DecodeError::Corrupt));
    assert_eq!(read, written[..first]);
    assert!(!decoder.is_complete());
    // A failed decoder stays failed.
    assert_eq!(decoder.summary().records as usize, first);

    // A wrong frame header is refused at once.
    let mut foreign = payload;
    foreign[0] ^= 1;
    let (read, result, _) = decode(Compression::Lz4, &foreign, 64);
    assert_eq!((read.len(), result), (0, Err(DecodeError::Corrupt)));
}

#[test]
fn plain_text_skips_lines_that_are_not_records() {
    let mut text = std::string::String::from("garbage line\n\n");
    let good = records(5, 2);
    for record in &good {
        super::text::write_line(&mut text, record).unwrap();
    }
    text.push_str("another bad one\n");
    text.push_str(&"x".repeat(5_000));
    text.push('\n');
    super::text::write_line(&mut text, &record(9, "after")).unwrap();
    let (read, result, decoder) = decode(Compression::None, text.as_bytes(), 50);
    assert_eq!(result, Ok(()));
    assert_eq!(read.len(), 3);
    assert_eq!(read[2].sequence, 9);
    assert_eq!(decoder.summary().skipped, 3);
    assert!(decoder.is_complete());
}
