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
///////////////////////////////////////////////////////////////////////////////
//! SDS011 wire protocol: command encoding and reply parsing.
//!
//! Derived from the message layer of the community
//! [`sds011-rs`](https://github.com/kenohassler/sds011-rs) crate by Keno
//! Hassler (MIT OR Apache-2.0), reduced to the commands the driver uses and
//! extended with explicit error values.
//!
//! Commands are 19 bytes: `AA B4 CMD DATA1 DATA2 0*10 FF FF CHK AB`, with
//! `CHK` the sum of bytes 2..=16 modulo 256. Replies and data frames are 10
//! bytes: `AA ID B2 B3 B4 B5 ID_L ID_H CHK AB`, with `CHK` the sum of bytes
//! 2..=7 modulo 256. All commands are broadcast (`FF FF`).

/// Length of a reply or data frame.
pub const REPLY_LEN: usize = 10;
/// Length of a command.
pub const COMMAND_LEN: usize = 19;
/// First byte of every frame.
pub const HEAD: u8 = 0xAA;

const TAIL: u8 = 0xAB;
/// Some firmware versions end the reply to a sleep/work command with `FF`.
const SLEEP_REPLY_ALT_TAIL: u8 = 0xFF;
const COMMAND_ID: u8 = 0xB4;
const DATA_ID: u8 = 0xC0;
const REPLY_ID: u8 = 0xC5;

const SUB_REPORTING_MODE: u8 = 2;
const SUB_QUERY: u8 = 4;
const SUB_SLEEP_WORK: u8 = 6;
const SUB_FIRMWARE: u8 = 7;

/// Command sent to the sensor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Stop reporting by itself, answer only to [`Command::Query`].
    QueryMode,
    /// Start the fan and the laser.
    Wake,
    /// Stop the fan and the laser.
    Sleep,
    /// Ask for a measurement.
    Query,
    /// Ask for the firmware date.
    Firmware,
}

fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |a, &x| a.wrapping_add(x))
}

/// Build the 19 bytes of `command`.
pub fn encode(command: Command) -> [u8; COMMAND_LEN] {
    let (sub, data1, data2) = match command {
        Command::QueryMode => (SUB_REPORTING_MODE, 1, 1),
        Command::Wake => (SUB_SLEEP_WORK, 1, 1),
        Command::Sleep => (SUB_SLEEP_WORK, 1, 0),
        Command::Query => (SUB_QUERY, 0, 0),
        Command::Firmware => (SUB_FIRMWARE, 0, 0),
    };
    let mut packet = [0u8; COMMAND_LEN];
    packet[0] = HEAD;
    packet[1] = COMMAND_ID;
    packet[2] = sub;
    packet[3] = data1;
    packet[4] = data2;
    packet[15] = 0xFF;
    packet[16] = 0xFF;
    packet[17] = checksum(&packet[2..17]);
    packet[18] = TAIL;
    packet
}

/// Content of a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// Measurement, in 0.1 µg/m³.
    Measurement {
        /// PM2.5.
        pm25: u16,
        /// PM10.
        pm10: u16,
    },
    /// Reply to [`Command::QueryMode`].
    ReportingMode {
        /// The sensor answers only to queries.
        query: bool,
    },
    /// Reply to [`Command::Wake`] or [`Command::Sleep`].
    Sleep {
        /// The fan and the laser run.
        work: bool,
    },
    /// Reply to [`Command::Firmware`].
    Firmware {
        /// Year since 2000.
        year: u8,
        /// Month.
        month: u8,
        /// Day.
        day: u8,
    },
}

/// A valid frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// Sensor ID.
    pub id: u16,
    /// Content.
    pub reply: Reply,
}

/// Why a frame was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// The checksum does not match.
    Checksum,
    /// The frame does not start with `AA` or end with `AB`.
    HeadTail,
    /// Unknown frame or command ID.
    Unknown(u8),
    /// A flag has a value other than 0 or 1.
    Flag(u8),
}

fn flag(value: u8) -> Result<bool, ParseError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(ParseError::Flag(other)),
    }
}

/// Validate and decode a frame.
pub fn parse(data: &[u8; REPLY_LEN]) -> Result<Frame, ParseError> {
    if checksum(&data[2..8]) != data[8] {
        return Err(ParseError::Checksum);
    }
    if data[0] != HEAD {
        return Err(ParseError::HeadTail);
    }

    let reply = match (data[1], data[2]) {
        (DATA_ID, _) => Reply::Measurement {
            pm25: u16::from_le_bytes([data[2], data[3]]),
            pm10: u16::from_le_bytes([data[4], data[5]]),
        },
        (REPLY_ID, SUB_REPORTING_MODE) => Reply::ReportingMode {
            query: flag(data[4])?,
        },
        (REPLY_ID, SUB_SLEEP_WORK) => Reply::Sleep {
            work: flag(data[4])?,
        },
        (REPLY_ID, SUB_FIRMWARE) => Reply::Firmware {
            year: data[3],
            month: data[4],
            day: data[5],
        },
        (REPLY_ID, sub) => return Err(ParseError::Unknown(sub)),
        (id, _) => return Err(ParseError::Unknown(id)),
    };

    let tail_ok = data[9] == TAIL
        || (matches!(reply, Reply::Sleep { .. }) && data[9] == SLEEP_REPLY_ALT_TAIL);
    if !tail_ok {
        return Err(ParseError::HeadTail);
    }
    Ok(Frame {
        id: u16::from_be_bytes([data[6], data[7]]),
        reply,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_match_the_control_protocol() {
        assert_eq!(
            encode(Command::Query),
            [
                0xAA, 0xB4, 0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF, 0x02, 0xAB
            ]
        );
        assert_eq!(
            encode(Command::Sleep),
            [
                0xAA, 0xB4, 0x06, 0x01, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF, 0x05, 0xAB
            ]
        );
        assert_eq!(encode(Command::Wake)[3..5], [0x01, 0x01]);
        assert_eq!(encode(Command::Wake)[17], 0x06);
        assert_eq!(encode(Command::QueryMode)[2..5], [0x02, 0x01, 0x01]);
        assert_eq!(encode(Command::QueryMode)[17], 0x02);
        assert_eq!(encode(Command::Firmware)[17], 0x05);
    }

    #[test]
    fn parses_measurement() {
        let frame = [0xAA, 0xC0, 0xD4, 0x04, 0x3A, 0x0A, 0xA1, 0x60, 0x1D, 0xAB];
        assert_eq!(
            parse(&frame),
            Ok(Frame {
                id: 0xA160,
                reply: Reply::Measurement {
                    pm25: 1236,
                    pm10: 2618
                }
            })
        );
    }

    #[test]
    fn parses_command_replies() {
        let query = [0xAA, 0xC5, 0x02, 0x01, 0x01, 0x00, 0xA1, 0x60, 0x05, 0xAB];
        assert_eq!(
            parse(&query).unwrap().reply,
            Reply::ReportingMode { query: true }
        );
        let active = [0xAA, 0xC5, 0x02, 0x00, 0x00, 0x00, 0xA1, 0x60, 0x03, 0xAB];
        assert_eq!(
            parse(&active).unwrap().reply,
            Reply::ReportingMode { query: false }
        );
        let sleep = [0xAA, 0xC5, 0x06, 0x01, 0x00, 0x00, 0xA1, 0x60, 0x08, 0xAB];
        assert_eq!(parse(&sleep).unwrap().reply, Reply::Sleep { work: false });
        let wake = [0xAA, 0xC5, 0x06, 0x01, 0x01, 0x00, 0xA1, 0x60, 0x09, 0xAB];
        assert_eq!(parse(&wake).unwrap().reply, Reply::Sleep { work: true });
    }

    #[test]
    fn accepts_alternative_tail_of_sleep_replies_only() {
        let sleep = [0xAA, 0xC5, 0x06, 0x01, 0x00, 0x00, 0xA1, 0x60, 0x08, 0xFF];
        assert_eq!(parse(&sleep).unwrap().reply, Reply::Sleep { work: false });
        let data = [0xAA, 0xC0, 0xD4, 0x04, 0x3A, 0x0A, 0xA1, 0x60, 0x1D, 0xFF];
        assert_eq!(parse(&data), Err(ParseError::HeadTail));
    }

    #[test]
    fn rejects_malformed_frames() {
        let good = [0xAA, 0xC0, 0xD4, 0x04, 0x3A, 0x0A, 0xA1, 0x60, 0x1D, 0xAB];

        let mut bad = good;
        bad[8] ^= 0x55;
        assert_eq!(parse(&bad), Err(ParseError::Checksum));

        let mut bad = good;
        bad[0] = 0x00;
        assert_eq!(parse(&bad), Err(ParseError::HeadTail));

        let mut bad = good;
        bad[9] = 0x00;
        assert_eq!(parse(&bad), Err(ParseError::HeadTail));

        let mut bad = good;
        bad[1] = 0x33;
        assert_eq!(parse(&bad), Err(ParseError::Unknown(0x33)));

        let mut unknown_sub = [0xAA, 0xC5, 0x09, 0x00, 0x00, 0x00, 0xA1, 0x60, 0, 0xAB];
        unknown_sub[8] = checksum(&unknown_sub[2..8]);
        assert_eq!(parse(&unknown_sub), Err(ParseError::Unknown(0x09)));

        let mut bad_flag = [0xAA, 0xC5, 0x06, 0x01, 0x02, 0x00, 0xA1, 0x60, 0, 0xAB];
        bad_flag[8] = checksum(&bad_flag[2..8]);
        assert_eq!(parse(&bad_flag), Err(ParseError::Flag(2)));
    }
}
