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
//! Which device a command talks to: the `--device` value and the choice among
//! the devices found.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use super::diagnostic::CliError;
use super::terminal::Io;
use crate::DiscoveredDevice;

/// How long the scan keeps looking after the first match, so that a second
/// device, which makes the choice ambiguous, has a chance to show up.
pub const SETTLE: Duration = Duration::from_secs(2);

/// Where a device is reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Bluetooth LE: a selector (ID, name, prefix or suffix of either), or
    /// none to take what is found.
    Ble(Option<String>),
    /// A TCP endpoint, `host:port`.
    Tcp(String),
}

impl Target {
    /// Interprets the `--device` value.
    ///
    /// A `ble://` or `tcp://` URI is taken as written. Without a scheme the
    /// transport follows from the content: `host:port` is TCP, anything else
    /// is a Bluetooth name or ID.
    pub fn parse(value: Option<&str>) -> Result<Self, CliError> {
        let Some(value) = value.map(str::trim) else {
            return Ok(Self::Ble(None));
        };
        if value.is_empty() {
            return Err(CliError::Usage("--device must not be empty".into()));
        }
        if let Some((scheme, rest)) = value.split_once("://") {
            return match scheme.to_ascii_lowercase().as_str() {
                "ble" if rest.is_empty() => Ok(Self::Ble(None)),
                "ble" => Ok(Self::Ble(Some(rest.to_owned()))),
                "tcp" if is_endpoint(rest) => Ok(Self::Tcp(rest.to_owned())),
                "tcp" => Err(CliError::Usage(format!(
                    "`{value}` is not a valid TCP target; expected tcp://HOST:PORT"
                ))),
                _ => Err(CliError::Usage(format!(
                    "unknown transport `{scheme}` in `{value}`; expected ble:// or tcp://"
                ))),
            };
        }
        if is_endpoint(value) {
            Ok(Self::Tcp(value.to_owned()))
        } else {
            Ok(Self::Ble(Some(value.to_owned())))
        }
    }
}

/// `ip:port`, `[ipv6]:port`, `localhost:port` or `dotted.host:port`.
fn is_endpoint(value: &str) -> bool {
    if value.parse::<SocketAddr>().is_ok() {
        return true;
    }
    let Some((host, port)) = value.rsplit_once(':') else {
        return false;
    };
    port.parse::<u16>().is_ok()
        && !host.is_empty()
        && !host.contains(':')
        && (host.parse::<Ipv4Addr>().is_ok() || host == "localhost" || host.contains('.'))
}

/// Devices a selector fits.
#[derive(Debug, PartialEq, Eq)]
pub enum Match {
    /// Exactly one, by index.
    Unique(usize),
    /// Several; none is better than the others.
    Many(Vec<usize>),
    /// None.
    None,
}

fn matches(tier: impl Fn(&DiscoveredDevice) -> bool, devices: &[DiscoveredDevice]) -> Vec<usize> {
    devices
        .iter()
        .enumerate()
        .filter(|(_, device)| tier(device))
        .map(|(index, _)| index)
        .collect()
}

/// Finds the devices `selector` names. The first of these that matches
/// anything decides: exact ID, exact name, then prefix or suffix of ID or
/// name (names and prefixes ignore case).
pub fn resolve(selector: &str, devices: &[DiscoveredDevice]) -> Match {
    let wanted = selector.to_lowercase();
    let name = |device: &DiscoveredDevice| device.name.as_deref().map(str::to_lowercase);
    let tiers: [&dyn Fn(&DiscoveredDevice) -> bool; 3] = [
        &|device| device.id == selector,
        &|device| name(device).is_some_and(|name| name == wanted),
        &|device| {
            let id = device.id.to_lowercase();
            [Some(id), name(device)]
                .into_iter()
                .flatten()
                .any(|text| text.starts_with(&wanted) || text.ends_with(&wanted))
        },
    ];
    for tier in tiers {
        let found = matches(tier, devices);
        match found.as_slice() {
            [] => {}
            [one] => return Match::Unique(*one),
            _ => return Match::Many(found),
        }
    }
    Match::None
}

/// Whether the scan can stop: the answer will not change by waiting more.
pub fn settled(selector: Option<&str>, devices: &[DiscoveredDevice], elapsed: Duration) -> bool {
    if devices.is_empty() {
        return false;
    }
    match selector {
        Some(selector) => {
            let exact = devices.iter().any(|device| device.id == selector)
                && matches!(resolve(selector, devices), Match::Unique(_));
            exact || (elapsed >= SETTLE && resolve(selector, devices) != Match::None)
        }
        None => elapsed >= SETTLE,
    }
}

/// The device to connect to: the one named by `selector`, the only one found,
/// or the one the user picks. Never an arbitrary one.
pub async fn choose(
    selector: Option<&str>,
    devices: &[DiscoveredDevice],
    io: &mut Io,
) -> Result<usize, CliError> {
    match selector {
        Some(selector) => match resolve(selector, devices) {
            Match::Unique(index) => Ok(index),
            Match::Many(indexes) => Err(CliError::Ambiguous {
                selector: Some(selector.to_owned()),
                devices: indexes.into_iter().map(|i| devices[i].clone()).collect(),
            }),
            Match::None if devices.is_empty() => Err(crate::Error::DeviceNotFound.into()),
            Match::None => Err(CliError::NoMatch {
                selector: selector.to_owned(),
                found: devices.to_vec(),
            }),
        },
        None => match devices {
            [] => Err(crate::Error::DeviceNotFound.into()),
            [_] => Ok(0),
            _ if io.interactive() => io.select_device(devices).await,
            _ => Err(CliError::Ambiguous {
                selector: None,
                devices: devices.to_vec(),
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, name: &str) -> DiscoveredDevice {
        DiscoveredDevice {
            id: id.to_owned(),
            name: Some(name.to_owned()),
            rssi: Some(-50),
        }
    }

    fn pair() -> Vec<DiscoveredDevice> {
        vec![
            device("hci0/dev_AA_00_11", "ALTRUIST-URBAN-A12F"),
            device("hci0/dev_BB_22_33", "ALTRUIST-URBAN-91CC"),
        ]
    }

    fn target(value: &str) -> Target {
        Target::parse(Some(value)).unwrap()
    }

    #[test]
    fn the_transport_follows_from_the_scheme_or_the_content() {
        assert_eq!(Target::parse(None).unwrap(), Target::Ble(None));
        assert_eq!(target("A12F"), Target::Ble(Some("A12F".into())));
        assert_eq!(
            target("ALTRUIST-URBAN-A12F"),
            Target::Ble(Some("ALTRUIST-URBAN-A12F".into()))
        );
        assert_eq!(
            target("hci0/dev_AA_00_11"),
            Target::Ble(Some("hci0/dev_AA_00_11".into()))
        );
        assert_eq!(
            target("AA:BB:CC:DD:EE:FF"),
            Target::Ble(Some("AA:BB:CC:DD:EE:FF".into()))
        );
        assert_eq!(
            target("ble://ALTRUIST-URBAN-A12F"),
            Target::Ble(Some("ALTRUIST-URBAN-A12F".into()))
        );
        assert_eq!(target("ble://"), Target::Ble(None));
        for tcp in [
            "192.168.0.23:22000",
            "[::1]:9300",
            "localhost:9300",
            "sim.local:1",
        ] {
            assert_eq!(target(tcp), Target::Tcp(tcp.into()), "{tcp}");
            assert_eq!(target(&format!("tcp://{tcp}")), Target::Tcp(tcp.into()));
        }
    }

    #[test]
    fn bad_targets_are_usage_errors() {
        for bad in [
            "",
            "tcp://",
            "tcp://host",
            "tcp://host:notaport",
            "http://x",
            "usb://1",
        ] {
            assert!(
                matches!(Target::parse(Some(bad)), Err(CliError::Usage(_))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn selectors_match_exactly_then_by_prefix_or_suffix() {
        let devices = pair();
        assert_eq!(resolve("hci0/dev_AA_00_11", &devices), Match::Unique(0));
        assert_eq!(resolve("altruist-urban-91cc", &devices), Match::Unique(1));
        assert_eq!(resolve("A12F", &devices), Match::Unique(0));
        assert_eq!(resolve("91cc", &devices), Match::Unique(1));
        assert_eq!(resolve("ALTRUIST-URBAN-9", &devices), Match::Unique(1));
        assert_eq!(resolve("hci0/dev_BB", &devices), Match::Unique(1));
        assert_eq!(resolve("ALTRUIST", &devices), Match::Many(vec![0, 1]));
        assert_eq!(resolve("hci0", &devices), Match::Many(vec![0, 1]));
        assert_eq!(resolve("nothing", &devices), Match::None);
    }

    #[test]
    fn an_exact_match_wins_over_longer_names_it_is_a_prefix_of() {
        let devices = vec![device("1", "ROHI"), device("2", "ROHI-2")];
        assert_eq!(resolve("ROHI", &devices), Match::Unique(0));
    }

    #[tokio::test]
    async fn the_choice_is_never_arbitrary() {
        let silent = || Io::scripted(Vec::new(), Vec::new(), false, &[]);

        let error = choose(None, &[], &mut silent()).await.unwrap_err();
        assert!(matches!(
            error,
            CliError::Device(crate::Error::DeviceNotFound)
        ));

        let one = vec![device("1", "ONLY")];
        assert_eq!(choose(None, &one, &mut silent()).await.unwrap(), 0);

        let error = choose(None, &pair(), &mut silent()).await.unwrap_err();
        assert!(matches!(error, CliError::Ambiguous { selector: None, .. }));

        let error = choose(Some("ALTRUIST"), &pair(), &mut silent())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            CliError::Ambiguous {
                selector: Some(_),
                ..
            }
        ));

        let error = choose(Some("nothing"), &pair(), &mut silent())
            .await
            .unwrap_err();
        assert!(matches!(error, CliError::NoMatch { .. }));

        assert_eq!(
            choose(Some("A12F"), &pair(), &mut silent()).await.unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn an_interactive_session_lets_the_user_choose() {
        let mut io = Io::scripted(Vec::new(), Vec::new(), true, &["7", "x", "2"]);
        assert_eq!(choose(None, &pair(), &mut io).await.unwrap(), 1);

        let mut io = Io::scripted(Vec::new(), Vec::new(), true, &[""]);
        assert!(matches!(
            choose(None, &pair(), &mut io).await,
            Err(CliError::Cancelled)
        ));
    }

    #[test]
    fn the_scan_ends_when_waiting_cannot_change_the_answer() {
        let devices = pair();
        let early = Duration::from_millis(250);
        assert!(!settled(None, &[], SETTLE));
        assert!(!settled(None, &devices, early));
        assert!(settled(None, &devices, SETTLE));
        assert!(settled(Some("hci0/dev_AA_00_11"), &devices, early));
        assert!(!settled(Some("A12F"), &devices, early));
        assert!(settled(Some("A12F"), &devices, SETTLE));
        assert!(!settled(Some("nothing"), &devices, SETTLE));
    }
}
