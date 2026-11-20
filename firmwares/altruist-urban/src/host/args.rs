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

//! Command line of `altruist-urban`.

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use clap::Parser;
use log::LevelFilter;

/// Altruist Urban as a host process.
///
/// Runs the Altruist Urban application on the host: file-backed configuration
/// flash, the Wi-Fi/IP platform of the host and the ROHI control protocol over
/// TCP and/or Bluetooth LE, depending on the build features. Connect with
/// `rohi --device tcp://127.0.0.1:9300 ...`.
#[derive(Parser, Debug, Clone)]
#[command(name = "altruist-urban", version)]
pub struct Args {
    /// Device name, reported in logs and diagnostics. Defaults to the name of
    /// the scenario, or `urban-std`.
    #[arg(long)]
    pub name: Option<String>,

    /// Address of the control and management listeners. The control listener
    /// follows the stored `control_tcp.bind_address` (loopback by default) unless
    /// this is given; the management listener is on loopback unless this is
    /// given.
    #[arg(long, value_name = "ADDRESS")]
    pub bind: Option<IpAddr>,

    /// Allow `--bind` to be a non-loopback address. The management interface has
    /// no authentication or encryption, and the control transport has none while
    /// it is open (`--control-open`): anyone who can reach them can reconfigure,
    /// provision and erase the device.
    #[arg(long)]
    pub unsafe_allow_remote: bool,

    /// Port of the ROHI control transport over TCP; 0 picks a free port. The
    /// stored `control_tcp.port` (9300 by default) applies unless this is given.
    #[cfg(feature = "std-tcp")]
    #[arg(long, value_name = "PORT")]
    pub control_port: Option<u16>,

    /// Start with the control endpoint over TCP open for this run, whatever the
    /// stored `control_tcp.access_mode` says: plaintext, unauthenticated records
    /// that need no key. For development on a loopback address; without it the
    /// endpoint is restricted by default: a client must complete the handshake with
    /// the pre-shared key and every record is encrypted.
    #[cfg(feature = "std-tcp")]
    #[arg(long)]
    pub control_open: bool,

    /// File with the 32-byte pre-shared key of the restricted TCP endpoint, as 64
    /// hexadecimal characters (whitespace around them is ignored) or as exactly 32
    /// raw bytes. It replaces the key stored on the device for this run and is
    /// never stored or logged; keep the file readable by its owner only. Without it
    /// the key stored on the device (provisioned over a protected link) applies.
    #[cfg(feature = "std-tcp")]
    #[arg(long, value_name = "FILE")]
    pub control_psk_file: Option<PathBuf>,

    /// Port of the development-management interface (fault injection and
    /// scenario control); 0 picks a free port. Defaults to the control port
    /// plus one, or a free port when the control port is 0.
    #[arg(long, value_name = "PORT")]
    pub manage_port: Option<u16>,

    /// The Bluetooth adapter the BLE peripheral uses, for example `hci0`.
    /// Defaults to the default adapter of BlueZ.
    #[cfg(feature = "std-ble")]
    #[arg(long, value_name = "ADAPTER")]
    pub ble_adapter: Option<String>,

    /// Configuration flash image. It survives restarts, and so does the
    /// persistent log, which is kept next to it in `<name>.log.flash`.
    #[arg(
        long,
        default_value = ".altruist-urban/device.flash",
        value_name = "FILE",
        conflicts_with = "ephemeral"
    )]
    pub storage: PathBuf,

    /// Keep the configuration flash and the persistent log in memory only.
    #[arg(long)]
    pub ephemeral: bool,

    /// Scenario file (TOML) with the initial state and a timeline of events.
    #[arg(long, value_name = "FILE")]
    pub scenario: Option<PathBuf>,

    /// Seed of every random sequence of the simulation: sensor jitter and the
    /// nonces of the encrypted telemetry sections and envelopes. The same scenario, seed and
    /// configuration produce the same simulated sequence.
    #[arg(long, value_name = "SEED")]
    pub seed: Option<u64>,

    /// Log level: error, warn, info, debug or trace. `RUST_LOG` overrides it.
    #[arg(long, default_value = "info", value_name = "LEVEL")]
    pub log_level: LevelFilter,

    /// Also write the READY line to this file.
    #[arg(long, value_name = "FILE")]
    pub ready_file: Option<PathBuf>,

    /// The ports of the previous run, passed on by a reboot of the device.
    #[arg(long, hide = true, num_args = 2, value_names = ["CONTROL", "MANAGE"])]
    pub reboot_ports: Option<Vec<u16>>,
}

impl Args {
    /// The control port of the TCP transport; `stored` is the one of the
    /// `control_tcp` section.
    #[cfg(feature = "std-tcp")]
    fn control(&self, stored: u16) -> u16 {
        self.control_port.unwrap_or(stored)
    }

    #[cfg(not(feature = "std-tcp"))]
    fn control(&self, _stored: u16) -> u16 {
        0
    }

    /// The address of the management listener, and of the control listener when
    /// `stored` (the `control_tcp.bind_address`) is not asked for.
    pub fn management_address(&self) -> IpAddr {
        self.bind.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
    }

    /// The address of the control listener.
    pub fn control_address(&self, stored: Ipv4Addr) -> IpAddr {
        self.bind.unwrap_or(IpAddr::V4(stored))
    }

    /// The control and management ports to bind; `stored` is the port of the
    /// `control_tcp` section.
    pub fn ports(&self, stored: u16) -> (u16, u16) {
        if let Some([control, manage]) = self.reboot_ports.as_deref() {
            return (*control, *manage);
        }
        let control = self.control(stored);
        let manage = self.manage_port.unwrap_or(match control {
            0 => 0,
            port => port.wrapping_add(1),
        });
        (control, manage)
    }

    /// The file of the persistent log flash: `device.flash` is paired with
    /// `device.log.flash`.
    pub fn log_storage(&self) -> PathBuf {
        self.storage.with_extension("log.flash")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_defaults_to_the_ignored_device_directory() {
        let args = Args::try_parse_from(["altruist-urban"]).unwrap();
        assert_eq!(args.storage, PathBuf::from(".altruist-urban/device.flash"));
        assert_eq!(
            args.log_storage(),
            PathBuf::from(".altruist-urban/device.log.flash")
        );
    }
}

/// Reads a pre-shared key of the control endpoint from `path`: 64 hexadecimal characters
/// (surrounding whitespace is ignored) or exactly 32 raw bytes. The file content is wiped from
/// memory before this returns.
///
/// # Errors
///
/// The I/O error, or `InvalidData` for any other content. The error never contains the content.
#[cfg(feature = "std-tcp")]
pub fn read_psk_file(path: &std::path::Path) -> std::io::Result<[u8; 32]> {
    use zeroize::Zeroize;

    let mut content = std::fs::read(path)?;
    let key = parse_psk(&content);
    content.zeroize();
    key.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "expected 64 hexadecimal characters or 32 raw bytes of key material",
        )
    })
}

#[cfg(feature = "std-tcp")]
fn parse_psk(content: &[u8]) -> Option<[u8; 32]> {
    let mut key = [0u8; 32];
    if let Ok(raw) = <[u8; 32]>::try_from(content) {
        key = raw;
    } else {
        let text = core::str::from_utf8(content).ok()?.trim();
        if text.len() != 64 || !text.is_ascii() {
            return None;
        }
        for (byte, pair) in key.iter_mut().zip(text.as_bytes().chunks(2)) {
            let pair = core::str::from_utf8(pair).ok()?;
            *byte = u8::from_str_radix(pair, 16).ok()?;
        }
    }
    // A key of zeros is not key material.
    key.iter().any(|byte| *byte != 0).then_some(key)
}

#[cfg(all(test, feature = "std-tcp"))]
mod psk_tests {
    use super::*;

    #[test]
    fn a_key_file_is_hex_or_raw_and_never_all_zeros() {
        let hex = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
        let expected = parse_psk(hex.as_bytes()).unwrap();
        assert_eq!(expected[0], 0x00);
        assert_eq!(expected[1], 0x11);
        assert_eq!(expected[31], 0xff);
        assert_eq!(parse_psk(format!("  {hex}\n").as_bytes()), Some(expected));
        assert_eq!(parse_psk(&expected), Some(expected));
        // Wrong length, wrong characters, zeros.
        assert_eq!(parse_psk(&hex.as_bytes()[..62]), None);
        assert_eq!(parse_psk(format!("{hex}00").as_bytes()), None);
        assert_eq!(parse_psk("zz".repeat(32).as_bytes()), None);
        assert_eq!(parse_psk(&[0u8; 32]), None);
        assert_eq!(parse_psk("00".repeat(32).as_bytes()), None);
    }
}
