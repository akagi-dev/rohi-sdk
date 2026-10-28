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
//! BLE central over `btleplug`.

use std::time::{Duration, Instant};

use btleplug::api::{
    Central, CharPropFlags, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::StreamExt;
use futures::stream::BoxStream;
use rohi_control_ble::uuid::{RX_CHARACTERISTIC_UUID, SERVICE_UUID, TX_CHARACTERISTIC_UUID};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::session::ClientTransport;

/// Chunk size that works at the default ATT MTU of 23 bytes. `btleplug` does
/// not report the negotiated MTU, so this is the safe choice for writes.
pub const DEFAULT_WRITE_PAYLOAD: usize = 20;

/// A ROHI device seen during a scan.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct DiscoveredDevice {
    /// Platform identifier, usable with [`BleLink::connect`].
    pub id: String,
    /// Advertised name.
    pub name: Option<String>,
    /// Signal strength in dBm.
    pub rssi: Option<i16>,
}

fn uuid(value: u128) -> Uuid {
    Uuid::from_u128(value)
}

async fn adapter() -> Result<Adapter> {
    let manager = Manager::new().await?;
    manager
        .adapters()
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| Error::Transport("no Bluetooth adapter".into()))
}

async fn describe(peripheral: &Peripheral) -> Result<DiscoveredDevice> {
    let properties = peripheral.properties().await?;
    Ok(DiscoveredDevice {
        id: peripheral.id().to_string(),
        name: properties.as_ref().and_then(|p| p.local_name.clone()),
        rssi: properties.as_ref().and_then(|p| p.rssi),
    })
}

/// Scans for devices advertising the ROHI Control Service.
pub async fn scan(duration: Duration) -> Result<Vec<DiscoveredDevice>> {
    scan_until(duration, |_, _| false).await
}

/// Scans for at most `max`, stopping earlier as soon as `done` returns `true`.
///
/// `done` sees the devices found so far and the time elapsed since the scan
/// started; it is evaluated about four times per second. Returns the devices
/// found when the scan ended.
pub async fn scan_until(
    max: Duration,
    mut done: impl FnMut(&[DiscoveredDevice], Duration) -> bool,
) -> Result<Vec<DiscoveredDevice>> {
    const POLL: Duration = Duration::from_millis(250);
    let adapter = adapter().await?;
    adapter
        .start_scan(ScanFilter {
            services: vec![uuid(SERVICE_UUID)],
        })
        .await?;
    let started = Instant::now();
    let found = loop {
        sleep(POLL.min(max.saturating_sub(started.elapsed()))).await;
        let mut found = Vec::new();
        for peripheral in adapter.peripherals().await? {
            found.push(describe(&peripheral).await?);
        }
        let elapsed = started.elapsed();
        if elapsed >= max || done(&found, elapsed) {
            break found;
        }
    };
    adapter.stop_scan().await?;
    Ok(found)
}

/// A connected device: one RX write characteristic, one TX notification stream.
pub struct BleLink {
    peripheral: Peripheral,
    rx: Characteristic,
    notifications: BoxStream<'static, Vec<u8>>,
    payload: usize,
}

impl BleLink {
    /// Connects to the device with the given identifier, or to the first ROHI
    /// device found when `id` is `None`.
    pub async fn connect(id: Option<&str>, scan_time: Duration) -> Result<Self> {
        let adapter = adapter().await?;
        adapter
            .start_scan(ScanFilter {
                services: vec![uuid(SERVICE_UUID)],
            })
            .await?;

        let peripheral = timeout(scan_time, async {
            loop {
                for peripheral in adapter.peripherals().await? {
                    if id.is_none_or(|id| peripheral.id().to_string() == id) {
                        let advertises = peripheral
                            .properties()
                            .await?
                            .is_some_and(|p| p.services.contains(&uuid(SERVICE_UUID)));
                        if advertises || id.is_some() {
                            return Ok::<_, Error>(peripheral);
                        }
                    }
                }
                sleep(Duration::from_millis(250)).await;
            }
        })
        .await
        .map_err(|_| Error::DeviceNotFound)??;
        adapter.stop_scan().await?;

        peripheral.connect().await?;
        peripheral.discover_services().await?;
        let characteristics = peripheral.characteristics();
        let find = |wanted: u128, property: CharPropFlags| {
            characteristics
                .iter()
                .find(|c| c.uuid == uuid(wanted) && c.properties.intersects(property))
                .cloned()
                .ok_or_else(|| {
                    Error::Transport(format!("characteristic {} not found", uuid(wanted)))
                })
        };
        let rx = find(
            RX_CHARACTERISTIC_UUID,
            CharPropFlags::WRITE | CharPropFlags::WRITE_WITHOUT_RESPONSE,
        )?;
        let tx = find(TX_CHARACTERISTIC_UUID, CharPropFlags::NOTIFY)?;
        let notifications = peripheral
            .notifications()
            .await?
            .filter(move |n| futures::future::ready(n.uuid == tx.uuid))
            .map(|n| n.value)
            .boxed();
        peripheral
            .subscribe(&find(TX_CHARACTERISTIC_UUID, CharPropFlags::NOTIFY)?)
            .await?;

        Ok(Self {
            peripheral,
            rx,
            notifications,
            payload: DEFAULT_WRITE_PAYLOAD,
        })
    }

    /// Overrides the chunk size, for platforms that negotiated a larger MTU.
    /// Values outside the protocol range are clamped.
    pub fn set_payload(&mut self, payload: usize) {
        self.payload = payload.clamp(DEFAULT_WRITE_PAYLOAD, rohi_net::MAX_BLE_PAYLOAD);
    }

    /// Closes the connection.
    pub async fn disconnect(self) -> Result<()> {
        self.peripheral.disconnect().await?;
        Ok(())
    }
}

impl ClientTransport for BleLink {
    fn max_payload(&self) -> usize {
        self.payload
    }

    async fn send(&mut self, chunk: &[u8]) -> Result<()> {
        self.peripheral
            .write(&self.rx, chunk, WriteType::WithResponse)
            .await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        self.notifications
            .next()
            .await
            .ok_or_else(|| Error::Transport("connection closed".into()))
    }
}
