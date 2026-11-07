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

use std::io::{Read, Write};
use std::vec::Vec;

use super::lz4::*;

fn sample(len: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut n = 0u32;
    while out.len() < len {
        n = n.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        out.extend_from_slice(
            std::format!(
                "{} 12{:03} WARN wifi: connected to home rssi -{}\n",
                n % 977,
                n % 1000,
                n % 90
            )
            .as_bytes(),
        );
    }
    out.truncate(len);
    out
}

fn random(len: usize) -> Vec<u8> {
    let mut n = 12345u32;
    (0..len)
        .map(|_| {
            n = n.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (n >> 24) as u8
        })
        .collect()
}

fn compress(input: &[u8]) -> Vec<u8> {
    let mut out = std::vec![0u8; input.len() + input.len() / 200 + 64];
    let len = compress_block(input, &mut out).unwrap();
    out.truncate(len);
    out
}

#[test]
fn xxh32_matches_the_reference_vectors() {
    assert_eq!(xxh32(b""), 0x02CC_5D05);
    assert_eq!(xxh32(b"a"), 0x550D_7456);
    assert_eq!(xxh32(b"abc"), 0x32D1_53FF);
    assert_eq!(
        xxh32(b"Nobody inspects the spammish repetition"),
        0xE229_3B2F
    );
}

#[test]
fn blocks_round_trip_through_our_codec() {
    for input in [
        std::vec![],
        b"a".to_vec(),
        b"short".to_vec(),
        b"twelve bytes".to_vec(),
        b"abcabcabcabcabcabcabcabcabcabcabc".to_vec(),
        std::vec![b'x'; 5_000],
        sample(1_000),
        sample(4_000),
        sample(60_000),
        random(3_000),
    ] {
        let packed = compress(&input);
        let mut out = std::vec![0u8; input.len()];
        let len = decompress_block(&packed, &mut out).unwrap();
        assert_eq!(&out[..len], input.as_slice(), "{} bytes", input.len());
    }
}

#[test]
fn log_text_gets_smaller() {
    let input = sample(4_096);
    assert!(compress(&input).len() < input.len() / 2);
}

#[test]
fn our_blocks_decode_with_a_reference_decoder() {
    for input in [
        sample(100),
        sample(4_000),
        std::vec![b'z'; 2_000],
        random(500),
    ] {
        let packed = compress(&input);
        let mut out = std::vec![0u8; input.len()];
        let len = lz4_flex::block::decompress_into(&packed, &mut out).unwrap();
        assert_eq!(&out[..len], input.as_slice());
    }
}

#[test]
fn reference_blocks_decode_with_our_decoder() {
    for input in [
        sample(100),
        sample(4_000),
        std::vec![b'z'; 2_000],
        random(500),
    ] {
        let packed = lz4_flex::block::compress(&input);
        let mut out = std::vec![0u8; input.len()];
        let len = decompress_block(&packed, &mut out).unwrap();
        assert_eq!(&out[..len], input.as_slice());
    }
}

#[test]
fn corrupt_blocks_are_rejected_not_trusted() {
    let packed = compress(&sample(2_000));
    let mut out = std::vec![0u8; 2_000];
    // Cut anywhere: an error or a short result, never a panic.
    for cut in 0..packed.len() {
        let _ = decompress_block(&packed[..cut], &mut out);
    }
    // A match before the start of the output.
    assert_eq!(
        decompress_block(&[0x04, 0x05, 0x00], &mut out),
        Err(Lz4Error::CorruptBlock)
    );
    // Output smaller than the data.
    let mut small = [0u8; 10];
    assert_eq!(
        decompress_block(&packed, &mut small),
        Err(Lz4Error::OutputTooSmall)
    );
}

fn frame_of(blocks: &[&[u8]]) -> Vec<u8> {
    let mut frame = frame_header().to_vec();
    for raw in blocks {
        let mut buffer = std::vec![0u8; raw.len() + BLOCK_OVERHEAD + raw.len() / 100 + 16];
        let len = encode_block(raw, &mut buffer).unwrap();
        frame.extend_from_slice(&buffer[..len]);
    }
    frame.extend_from_slice(&END_MARK);
    frame
}

#[test]
fn a_frame_is_a_standard_lz4_file() {
    let first = sample(3_000);
    let second = sample(1_500);
    let incompressible = random(700);
    let frame = frame_of(&[&first, &second, &incompressible]);

    let mut decoded = Vec::new();
    lz4_flex::frame::FrameDecoder::new(frame.as_slice())
        .read_to_end(&mut decoded)
        .unwrap();
    let expected: Vec<u8> = [first, second, incompressible].concat();
    assert_eq!(decoded, expected);
}

#[test]
fn a_standard_frame_is_not_ours_but_ours_is_checked() {
    let mut reference = Vec::new();
    let mut encoder = lz4_flex::frame::FrameEncoder::new(&mut reference);
    encoder.write_all(&sample(500)).unwrap();
    encoder.finish().unwrap();
    // The frame descriptor of the reference encoder differs from ours.
    assert_eq!(check_frame_header(&reference), Err(Lz4Error::BadHeader));
    assert_eq!(check_frame_header(&frame_header()), Ok(()));
    assert_eq!(check_frame_header(&[0; 3]), Err(Lz4Error::BadHeader));
}

#[test]
fn blocks_are_read_back_one_by_one_and_stop_at_damage() {
    let raws = [sample(2_000), random(300), sample(900)];
    let frame = frame_of(&[&raws[0], &raws[1], &raws[2]]);
    let mut rest = &frame[FRAME_HEADER_LEN..];
    let mut out = std::vec![0u8; 4_096];
    for raw in &raws {
        let (block, used) = next_block(rest).unwrap().unwrap();
        let len = block.decode(&mut out).unwrap();
        assert_eq!(&out[..len], raw.as_slice());
        rest = &rest[used..];
    }
    assert_eq!(next_block(rest), Ok(None));

    // A flipped bit and a torn tail both fail the checksum or the length.
    let mut damaged = frame.clone();
    damaged[FRAME_HEADER_LEN + 30] ^= 0x10;
    assert_eq!(
        next_block(&damaged[FRAME_HEADER_LEN..]),
        Err(Lz4Error::CorruptBlock)
    );
    let torn = &frame[FRAME_HEADER_LEN..FRAME_HEADER_LEN + 40];
    assert_eq!(next_block(torn), Err(Lz4Error::CorruptBlock));
    assert_eq!(next_block(&[]), Err(Lz4Error::CorruptBlock));
}

#[test]
fn encoding_respects_the_output_bound() {
    let raw = sample(1_000);
    let mut small = [0u8; 20];
    assert_eq!(
        encode_block(&raw, &mut small),
        Err(Lz4Error::OutputTooSmall)
    );
    let mut tiny = [0u8; 6];
    assert_eq!(encode_block(&raw, &mut tiny), Err(Lz4Error::OutputTooSmall));
    assert_eq!(
        encode_block(&std::vec![0u8; MAX_BLOCK_INPUT + 1], &mut [0u8; 8]),
        Err(Lz4Error::InputTooLarge)
    );
    // Incompressible data is stored, within raw length plus the overhead.
    let noise = random(512);
    let mut out = std::vec![0u8; 512 + BLOCK_OVERHEAD];
    let len = encode_block(&noise, &mut out).unwrap();
    assert_eq!(len, 512 + BLOCK_OVERHEAD);
    assert!(next_block(&out).unwrap().unwrap().0.stored);
}
