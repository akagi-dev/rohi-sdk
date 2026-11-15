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
//! Telemetry delivery of the device: routes, the packet builder and the
//! publisher task.
//!
//! The routes are one private section kept by the configuration owner (see
//! [`crate::config`]); a commit publishes them to [`state`] in the same step.
//! [`UrbanPackets`] builds sealed packets: it captures the local sensors and
//! the location once, applies a telemetry profile and lets the Robonomics
//! format encrypt and sign the result with the device identity. The
//! publisher ([`run`]) and the `packet_get` export use it; the publisher only
//! reads snapshots, so whatever the uplink does it can neither delay nor stop
//! the sensors.

use core::fmt;

use embassy_time::Delay;
use log::{info, warn};
use rohi_sdk::connectivity::{
    BuildFailure, ConnectivitySection, ConnectivityService, ConnectivityState, ConnectivityStore,
    HttpTransport, PacketExporter, PacketProvider, Publisher, RouteSet, StoreError,
};
use rohi_sdk::net::{IpBackend, NetworkHandle};
use rohi_sdk::sensors::runtime::MeasurementHandle;
use rohi_sdk::telemetry::adapters::{LocationTelemetrySource, SensorsTelemetrySource};
use rohi_sdk::telemetry::{
    PacketFormat, ProfileStamp, SealError, TelemetrySnapshot, capture, prepare,
};
use rohi_sdk::telemetry_robonomics::wire::limits::MAX_ENVELOPE_LEN;
use rohi_sdk::telemetry_robonomics::{Entropy, EntropyError, RobonomicsFormat};
use rohi_sdk::time::{Clock, EmbassyClock};

use crate::bindings::PrivateBinding;
use crate::config::{ConfigHandle, ServiceError};
use crate::identity::StaticSigner;
use crate::tasks::RawMutex;
use crate::{location, telemetry, time};

/// Packets queued per route while the uplink is down. When the outbox is full
/// new packets are skipped, never queued over old ones.
pub const OUTBOX_DEPTH: usize = 2;
/// Sealed packets the pool holds: queued, being sent or being built.
pub const POOL_SLOTS: usize = 3;
/// Packet streams `packet_get` keeps open at once.
pub const EXPORT_STREAMS: usize = 1;
/// Size of a sealed packet buffer: the largest envelope of the format.
pub const PACKET_BYTES: usize = MAX_ENVELOPE_LEN;
/// Observations a capture holds: one slot for every measurement of the
/// catalog, so every sensor and the location fit together. A smaller snapshot
/// would discard the whole dataset of a source (a capture is atomic per
/// source), so this is never a tuning knob below the catalog size.
pub const SNAPSHOT_OBSERVATIONS: usize = rohi_sdk::telemetry::CATALOG.len();
/// Budget of one source during a capture, in milliseconds.
const SOURCE_TIMEOUT_MS: u32 = 2_000;

static STATE: ConnectivityState<RawMutex> = ConnectivityState::new();

/// The effective routes and the runtime status of the publisher.
pub fn state() -> &'static ConnectivityState<RawMutex> {
    &STATE
}

/// Fills a buffer with unpredictable bytes; `false` when no entropy is
/// available. A true random source on hardware, a seeded one for a
/// reproducible simulation. It must never repeat a value: it feeds the nonces
/// of encrypted sections.
pub type EntropyFn = fn(&mut [u8]) -> bool;

#[derive(Clone, Copy)]
struct FnEntropy(EntropyFn);

impl Entropy for FnEntropy {
    fn fill(&mut self, dst: &mut [u8]) -> Result<(), EntropyError> {
        if (self.0)(dst) {
            Ok(())
        } else {
            Err(EntropyError)
        }
    }
}

/// The stored routes: published to [`state`] when committed, removed or loaded
/// at boot.
pub struct ConnectivityBinding;

impl PrivateBinding for ConnectivityBinding {
    type Section = ConnectivitySection;

    fn publish(section: Option<&ConnectivitySection>) {
        match section {
            Some(section) => {
                info!(
                    "Connectivity: {} route(s), generation {}",
                    section.0.routes.len(),
                    section.0.generation
                );
                STATE.publish(&section.0);
            }
            None => {
                info!("Connectivity: no routes");
                STATE.publish(&RouteSet {
                    generation: STATE.generation(),
                    ..RouteSet::default()
                });
            }
        }
    }

    fn unreadable(error: &dyn fmt::Display) {
        warn!(
            "Connectivity: stored routes unreadable, nothing is sent: {}",
            error
        );
        STATE.mark_unreadable();
    }
}

/// [`ConnectivityStore`] over the configuration owner task.
#[derive(Clone, Copy)]
pub struct StoredRoutes(ConfigHandle);

fn store_error(error: ServiceError) -> StoreError {
    match error {
        ServiceError::Unavailable(_) => StoreError::Unavailable,
        _ => StoreError::Internal,
    }
}

impl ConnectivityStore for StoredRoutes {
    async fn load(&self) -> Result<RouteSet, StoreError> {
        match self.0.get::<ConnectivitySection>().await {
            Ok(section) => Ok(section.map(|section| section.0).unwrap_or_default()),
            Err(error) => Err(store_error(error)),
        }
    }

    async fn commit(&self, candidate: &RouteSet) -> Result<(), StoreError> {
        // The owner task commits and publishes in one step.
        self.0
            .set(&ConnectivitySection(candidate.clone()))
            .await
            .map_err(store_error)
    }
}

/// Builds sealed packets of the telemetry profiles of the device.
#[derive(Clone, Copy)]
pub struct UrbanPackets {
    measurements: MeasurementHandle<'static, RawMutex>,
    entropy: EntropyFn,
}

/// The packet builder over `measurements`, with `entropy` for the nonces.
pub const fn packets(
    measurements: MeasurementHandle<'static, RawMutex>,
    entropy: EntropyFn,
) -> UrbanPackets {
    UrbanPackets {
        measurements,
        entropy,
    }
}

fn map_seal(error: SealError) -> BuildFailure {
    match error {
        SealError::NothingToPackage => BuildFailure::NothingToPackage,
        SealError::TimeUnavailable => BuildFailure::TimeUnavailable,
        SealError::IdentityUnavailable => BuildFailure::IdentityUnavailable,
        SealError::EntropyUnavailable => BuildFailure::EntropyUnavailable,
        SealError::BufferTooSmall | SealError::TooLarge => BuildFailure::TooLarge,
        SealError::Unsupported(_) | SealError::Encryption | SealError::Signing => {
            BuildFailure::Failed
        }
    }
}

impl UrbanPackets {
    async fn capture_snapshot(
        &self,
    ) -> Result<TelemetrySnapshot<SNAPSHOT_OBSERVATIONS>, BuildFailure> {
        let mut sources = (
            SensorsTelemetrySource::new(self.measurements),
            LocationTelemetrySource::new(location::shared(), || EmbassyClock.now()),
        );
        Ok(capture(&mut sources, &EmbassyClock, &mut Delay, SOURCE_TIMEOUT_MS).await)
    }

    async fn seal(
        &self,
        snapshot: &TelemetrySnapshot<SNAPSHOT_OBSERVATIONS>,
        stamp: &ProfileStamp,
        out: &mut [u8],
    ) -> Result<usize, BuildFailure> {
        let profile = telemetry::state()
            .with_profile(stamp.id.as_str(), |profile| profile.clone())
            .filter(|profile| {
                profile.incarnation == stamp.incarnation && profile.revision == stamp.revision
            })
            .ok_or(BuildFailure::ProfileChanged)?;
        let key = StaticSigner;
        let mut format =
            RobonomicsFormat::new(&key, FnEntropy(self.entropy), time::service(), EmbassyClock);
        let sections = prepare(snapshot, &profile, &format.supported())
            .map_err(|_| BuildFailure::NothingToPackage)?;
        let len = format
            .seal(&sections, out)
            .await
            .map_err(map_seal)?
            .info()
            .length;
        // A policy edit that completed while sealing revokes the packet.
        if !telemetry::state().is_current(stamp) {
            return Err(BuildFailure::ProfileChanged);
        }
        Ok(len)
    }
}

impl PacketProvider for UrbanPackets {
    type Snapshot = TelemetrySnapshot<SNAPSHOT_OBSERVATIONS>;

    fn resolve(&self, profile: &str) -> Option<ProfileStamp> {
        telemetry::state().resolve(profile)
    }

    fn is_current(&self, stamp: &ProfileStamp) -> bool {
        telemetry::state().is_current(stamp)
    }

    async fn capture(&mut self) -> Result<Self::Snapshot, BuildFailure> {
        self.capture_snapshot().await
    }

    async fn build(
        &mut self,
        snapshot: &Self::Snapshot,
        stamp: &ProfileStamp,
        out: &mut [u8],
    ) -> Result<usize, BuildFailure> {
        self.seal(snapshot, stamp, out).await
    }

    async fn changed(&self) {
        telemetry::state().changed().await;
    }
}

impl PacketExporter for UrbanPackets {
    fn now_ms(&self) -> u64 {
        EmbassyClock.now().millis
    }

    fn resolve(&self, profile: &str) -> Option<ProfileStamp> {
        telemetry::state().resolve(profile)
    }

    async fn export(
        &self,
        profile: &str,
        out: &mut [u8],
    ) -> Result<(ProfileStamp, usize), BuildFailure> {
        let stamp = telemetry::state()
            .resolve(profile)
            .ok_or(BuildFailure::ProfileChanged)?;
        let snapshot = self.capture_snapshot().await?;
        let len = self.seal(&snapshot, &stamp, out).await?;
        Ok((stamp, len))
    }
}

/// The `connectivity` service as the firmware composes it.
pub type UrbanConnectivity = ConnectivityService<
    'static,
    RawMutex,
    StoredRoutes,
    UrbanPackets,
    EXPORT_STREAMS,
    PACKET_BYTES,
>;

/// The service over the routes stored behind `config`.
pub fn service(config: ConfigHandle, packets: UrbanPackets) -> UrbanConnectivity {
    ConnectivityService::new(StoredRoutes(config), packets, &STATE)
}

/// Runs the publisher; never returns. The platform runs it in its connectivity
/// task. It reads snapshots only.
pub async fn run<I: IpBackend>(network: NetworkHandle<'static, I>, packets: UrbanPackets) -> ! {
    let transport = HttpTransport::new(network, Delay);
    let mut publisher: Publisher<
        '_,
        RawMutex,
        UrbanPackets,
        _,
        Delay,
        EmbassyClock,
        OUTBOX_DEPTH,
        POOL_SLOTS,
        PACKET_BYTES,
    > = Publisher::new(&STATE, packets, transport, Delay, EmbassyClock);
    publisher.run().await
}
