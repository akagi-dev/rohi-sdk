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

use core::cmp::Ordering;

use heapless::Vec;

use crate::{MAX_WIFI_SCAN_RESULTS, WifiSsid};

/// Authentication mode of a scanned network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiSecurity {
    Open,
    Wep,
    Wpa,
    Wpa2,
    WpaWpa2,
    Wpa3,
    Wpa2Wpa3,
    /// WPA enterprise (802.1X); not supported for connecting.
    Enterprise,
    Unknown,
}

/// One network found by a scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiNetwork {
    pub ssid: WifiSsid,
    pub rssi_dbm: i8,
    pub security: WifiSecurity,
    pub channel: u8,
}

impl WifiNetwork {
    /// Strongest first; ties are broken by SSID and channel so that the
    /// selected subset is deterministic.
    fn order(&self, other: &Self) -> Ordering {
        other
            .rssi_dbm
            .cmp(&self.rssi_dbm)
            .then_with(|| self.ssid.as_bytes().cmp(other.ssid.as_bytes()))
            .then_with(|| self.channel.cmp(&other.channel))
    }
}

/// Bounded scan result, strongest network first.
pub type WifiScanResults = Vec<WifiNetwork, MAX_WIFI_SCAN_RESULTS>;

/// Builds [`WifiScanResults`] from an arbitrary number of observations
/// without allocating.
///
/// - hidden networks (empty SSID) are skipped;
/// - several access points with the same SSID collapse to the strongest one;
/// - when more distinct networks are seen than fit, the strongest are kept.
#[derive(Debug, Default)]
pub struct ScanCollector {
    results: WifiScanResults,
}

impl ScanCollector {
    pub const fn new() -> Self {
        Self {
            results: Vec::new(),
        }
    }

    pub fn insert(&mut self, network: WifiNetwork) {
        if network.ssid.is_empty() {
            return;
        }

        if let Some(existing) = self
            .results
            .iter_mut()
            .find(|known| known.ssid == network.ssid)
        {
            if network.order(existing) == Ordering::Less {
                *existing = network;
            }
            return;
        }

        if let Err(network) = self.results.push(network) {
            // Full: replace the weakest entry if the new one is stronger.
            let weakest = self
                .results
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.order(b))
                .map(|(index, _)| index);
            if let Some(index) = weakest
                && network.order(&self.results[index]) == Ordering::Less
            {
                self.results[index] = network;
            }
        }
    }

    /// Returns the results sorted strongest first.
    pub fn finish(mut self) -> WifiScanResults {
        self.results.sort_unstable_by(WifiNetwork::order);
        self.results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(ssid: &str, rssi_dbm: i8, channel: u8) -> WifiNetwork {
        WifiNetwork {
            ssid: WifiSsid::from_bytes(ssid.as_bytes()).unwrap(),
            rssi_dbm,
            security: WifiSecurity::Wpa2,
            channel,
        }
    }

    fn collect(networks: &[WifiNetwork]) -> WifiScanResults {
        let mut collector = ScanCollector::new();
        for network in networks {
            collector.insert(network.clone());
        }
        collector.finish()
    }

    #[test]
    fn empty_scan() {
        assert!(collect(&[]).is_empty());
    }

    #[test]
    fn single_network() {
        let results = collect(&[net("office", -50, 6)]);
        assert_eq!(results.as_slice(), &[net("office", -50, 6)]);
    }

    #[test]
    fn sorted_strongest_first() {
        let results = collect(&[net("c", -80, 1), net("a", -40, 6), net("b", -60, 11)]);
        let order: [&str; 3] = [
            results[0].ssid.as_str().unwrap(),
            results[1].ssid.as_str().unwrap(),
            results[2].ssid.as_str().unwrap(),
        ];
        assert_eq!(order, ["a", "b", "c"]);
    }

    #[test]
    fn duplicate_ssid_keeps_strongest() {
        let results = collect(&[net("office", -70, 1), net("office", -45, 11)]);
        assert_eq!(results.as_slice(), &[net("office", -45, 11)]);
    }

    #[test]
    fn hidden_networks_are_skipped() {
        let hidden = WifiNetwork {
            ssid: WifiSsid::empty(),
            rssi_dbm: -30,
            security: WifiSecurity::Wpa2,
            channel: 1,
        };
        assert!(collect(&[hidden]).is_empty());
    }

    #[test]
    fn overflow_keeps_strongest_n_deterministically() {
        use std::{format, vec::Vec as StdVec};
        extern crate std;

        let total = MAX_WIFI_SCAN_RESULTS + 10;
        let mut all: StdVec<WifiNetwork> = (0..total)
            .map(|i| net(&format!("net{i:02}"), -90 + i as i8, 1))
            .collect();

        let forward = collect(&all);
        all.reverse();
        let backward = collect(&all);

        assert_eq!(forward.len(), MAX_WIFI_SCAN_RESULTS);
        assert_eq!(forward, backward);
        // The strongest network is first, the weakest kept one is the 16th strongest.
        assert_eq!(forward[0].rssi_dbm, -90 + (total - 1) as i8);
        assert_eq!(
            forward[MAX_WIFI_SCAN_RESULTS - 1].rssi_dbm,
            -90 + (total - MAX_WIFI_SCAN_RESULTS) as i8
        );
    }

    #[test]
    fn equal_rssi_tie_break_is_stable() {
        let a = collect(&[net("b", -50, 1), net("a", -50, 1)]);
        let b = collect(&[net("a", -50, 1), net("b", -50, 1)]);
        assert_eq!(a, b);
        assert_eq!(a[0].ssid.as_str(), Some("a"));
    }

    #[test]
    fn security_is_preserved() {
        let mut open = net("cafe", -60, 3);
        open.security = WifiSecurity::Open;
        assert_eq!(collect(&[open])[0].security, WifiSecurity::Open);
    }
}
