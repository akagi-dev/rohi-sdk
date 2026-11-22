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
use std::vec::Vec;

use super::*;

fn bytes(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 7 + 3) as u8).collect()
}

fn frames(payload: &[u8]) -> Vec<Vec<u8>> {
    let framer = Framer::new(payload).unwrap();
    (0..framer.count())
        .map(|i| {
            let mut out = [0u8; MTU];
            let len = framer.frame(i, &mut out);
            out[..len].to_vec()
        })
        .collect()
}

/// What a conforming receiver does: validates every frame and puts the payload back together.
fn reassemble(frames: &[Vec<u8>]) -> Vec<u8> {
    let first = &frames[0];
    if first[0] == CONTROL_SINGLE {
        assert_eq!(frames.len(), 1);
        return first[1..].to_vec();
    }
    let mut payload = Vec::new();
    let id: [u8; 6] = first[1..7].try_into().unwrap();
    let count = usize::from(first[7] >> 4) + 1;
    assert_eq!(frames.len(), count);
    for (index, frame) in frames.iter().enumerate() {
        assert!(frame.len() <= MTU);
        assert_eq!(frame[0], CONTROL_FRAGMENT);
        assert_eq!(frame[1..7], id);
        assert_eq!(usize::from(frame[7] & 0x0f), index);
        assert_eq!(usize::from(frame[7] >> 4) + 1, count);
        let body = &frame[FRAGMENT_HEADER..];
        assert!((1..=FRAGMENT_BODY_MAX).contains(&body.len()));
        if index + 1 < count {
            assert_eq!(
                body.len(),
                FRAGMENT_BODY_MAX,
                "a non-final fragment is full"
            );
        }
        payload.extend_from_slice(body);
    }
    assert_eq!(
        id,
        Sha256::digest(&payload)[..6],
        "message id is the digest prefix"
    );
    payload
}

#[test]
fn the_limits_are_those_of_the_specification() {
    assert_eq!((MTU, SINGLE_MAX, FRAGMENT_BODY_MAX), (219, 218, 211));
    assert_eq!((MAX_FRAGMENTS, MAX_PAYLOAD), (16, 3376));
}

#[test]
fn zero_and_oversized_payloads_are_refused_before_any_frame() {
    assert_eq!(Framer::new(&[]).unwrap_err(), FrameError::Empty);
    assert_eq!(Framer::new(&bytes(3377)).unwrap_err(), FrameError::TooLarge);
    assert_eq!(Framer::new(&bytes(9000)).unwrap_err(), FrameError::TooLarge);
    assert!(Framer::new(&bytes(3376)).is_ok());
}

#[test]
fn boundary_lengths_use_single_or_the_minimum_number_of_fragments() {
    for (len, count) in [
        (1, 1),
        (218, 1),
        (219, 2),
        (211 * 2, 2),
        (211 * 2 + 1, 3),
        (211 * 15, 15),
        (211 * 15 + 1, 16),
        (3376, 16),
    ] {
        let payload = bytes(len);
        let frames = frames(&payload);
        assert_eq!(frames.len(), count, "len {len}");
        assert_eq!(reassemble(&frames), payload, "len {len}");
        let first = frames[0][0];
        assert_eq!(first, if count == 1 { 0x01 } else { 0x41 }, "len {len}");
    }
}

#[test]
fn every_length_round_trips_byte_for_byte() {
    for len in (1..=3376)
        .step_by(5)
        .chain([217, 218, 219, 220, 421, 422, 423, 3375, 3376])
    {
        let payload = bytes(len);
        assert_eq!(reassemble(&frames(&payload)), payload, "len {len}");
    }
}

#[test]
fn the_fragment_header_is_laid_out_as_specified() {
    // SHA-256 of 220 bytes of 0xab is fixed; the id is its first six octets in digest order.
    let payload = [0xabu8; 220];
    let framer = Framer::new(&payload).unwrap();
    let digest = Sha256::digest(payload);
    assert_eq!(framer.message_id(), digest[..6]);
    let frames = frames(&payload);
    assert_eq!(
        frames[0][..8],
        [
            0x41, digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], 0x10
        ]
    );
    assert_eq!(frames[1][7], 0x11);
    assert_eq!(frames[1].len(), 8 + 9);

    // The second fragment of three: count 3, index 1 is 0x21.
    let three = frames_of_three();
    assert_eq!(three[1][7], 0x21);
}

fn frames_of_three() -> Vec<Vec<u8>> {
    frames(&bytes(211 * 2 + 5))
}

#[test]
fn the_known_message_id_example_of_the_specification_digest_order() {
    // SHA256("") starts e3 b0 c4 42 98 fc; the framer must copy octets, not interpret an integer.
    assert_eq!(
        Sha256::digest([])[..6],
        [0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc]
    );
    let framer = Framer::new(&[1u8; 400]).unwrap();
    assert_eq!(framer.message_id(), Sha256::digest([1u8; 400])[..6]);
}

#[test]
fn an_index_outside_the_frames_is_empty() {
    let framer = Framer::new(&[1, 2, 3]).unwrap();
    let mut out = [0u8; MTU];
    assert_eq!(framer.frame(1, &mut out), 0);
}
