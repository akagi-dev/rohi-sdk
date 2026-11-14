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
//! Prepared sections: borrowed views of one snapshot selected by a profile.
//!
//! Sections never copy observations: a section is a selection over the
//! snapshot, so everything derived from one snapshot refers to the same
//! recorded values, and nothing outlives the snapshot.

use heapless::Vec;

use crate::observation::Observation;
use crate::profile::{PacketProfile, PacketProfileId, RecipientAddress};
use crate::selection::{DataSelection, Selection};
use crate::snapshot::TelemetrySnapshot;

/// Identifies the exact policy a packet was built under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileStamp {
    /// The profile.
    pub id: PacketProfileId,
    /// The incarnation of the profile.
    pub incarnation: u32,
    /// The revision of the policy.
    pub revision: u32,
}

/// The observations of a snapshot that one section of a profile selects.
#[derive(Clone, Copy, Debug)]
pub struct SectionView<'s, const N: usize> {
    snapshot: &'s TelemetrySnapshot<N>,
    sources: crate::selection::SourceSelection,
    measurements: Selection,
}

impl<'s, const N: usize> SectionView<'s, N> {
    fn new(
        snapshot: &'s TelemetrySnapshot<N>,
        selection: &DataSelection,
        available: &Selection,
    ) -> Self {
        Self {
            snapshot,
            sources: selection.sources,
            measurements: selection.measurements.resolve(available),
        }
    }

    /// The selected observations, in snapshot order. Selected measurements
    /// the snapshot does not have are simply absent.
    pub fn observations(&self) -> impl Iterator<Item = &'s Observation> + '_ {
        let (sources, measurements) = (self.sources, self.measurements);
        self.snapshot
            .observations
            .iter()
            .filter(move |o| sources.includes(o.source) && measurements.contains(o.measurement))
    }

    /// Whether the section selects no observation of this snapshot.
    pub fn is_empty(&self) -> bool {
        self.observations().next().is_none()
    }
}

/// The section of one recipient, to be encrypted by the format.
#[derive(Clone, Copy, Debug)]
pub struct PrivateSectionView<'s, const N: usize> {
    /// Who receives it.
    pub recipient: RecipientAddress,
    /// What it holds, as logical plaintext.
    pub view: SectionView<'s, N>,
}

/// Why no sections could be prepared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrepareError {
    /// Nothing the profile selects is in the snapshot: no packet, not an
    /// empty packet.
    NothingToPackage,
}

/// The sections of one profile over one snapshot.
#[derive(Clone, Debug)]
pub struct PreparedSections<'s, const N: usize, const RECIPIENTS: usize> {
    /// The policy these sections were prepared under.
    pub profile: ProfileStamp,
    /// The public section, plaintext in the packet.
    pub public: SectionView<'s, N>,
    /// Non-empty private sections, one per recipient.
    pub private: Vec<PrivateSectionView<'s, N>, RECIPIENTS>,
    /// When the snapshot was captured.
    pub captured_at: rohi_time::MonotonicTimestamp,
}

/// Applies the policy of `profile` to `snapshot` without reading anything
/// again. `available` is what the packet format can encode (resolves `all`).
///
/// Empty private sections are omitted: there is nothing to encrypt.
///
/// # Errors
///
/// [`PrepareError::NothingToPackage`] when every section is empty.
pub fn prepare<'s, const N: usize, const R: usize>(
    snapshot: &'s TelemetrySnapshot<N>,
    profile: &PacketProfile<R>,
    available: &Selection,
) -> Result<PreparedSections<'s, N, R>, PrepareError> {
    let public = SectionView::new(snapshot, &profile.public, available);
    let mut private = Vec::new();
    for policy in &profile.private {
        let view = SectionView::new(snapshot, &policy.selection, available);
        if !view.is_empty() {
            // Bounded by the profile's own capacity.
            let _ = private.push(PrivateSectionView {
                recipient: policy.recipient,
                view,
            });
        }
    }
    if public.is_empty() && private.is_empty() {
        return Err(PrepareError::NothingToPackage);
    }
    Ok(PreparedSections {
        profile: ProfileStamp {
            id: profile.id.clone(),
            incarnation: profile.incarnation,
            revision: profile.revision,
        },
        public,
        private,
        captured_at: snapshot.captured_at,
    })
}

#[cfg(test)]
mod tests {
    use rohi_time::MonotonicTimestamp;

    use super::*;
    use crate::catalog::{BMX280_TEMPERATURE, LOCATION, SDS011_PM10, all_cataloged};
    use crate::observation::{GeoPoint, ObservationValue, SourceId};
    use crate::profile::{FormatCapabilities, PacketFormatId, TelemetryConfig};

    struct Caps;
    impl FormatCapabilities for Caps {
        fn supported(&self, _: PacketFormatId) -> Selection {
            all_cataloged()
        }
        fn accepts_recipient(&self, _: PacketFormatId, _: &RecipientAddress) -> bool {
            true
        }
    }

    fn snapshot() -> TelemetrySnapshot<8> {
        let at = MonotonicTimestamp::from_millis(5);
        let mut snapshot = TelemetrySnapshot::new(MonotonicTimestamp::from_millis(9));
        let mut writer = snapshot.writer(SourceId(1));
        writer.number(BMX280_TEMPERATURE, 20.0, at).unwrap();
        writer.number(SDS011_PM10, 7.0, at).unwrap();
        writer.finish(Ok(()));
        let mut writer = snapshot.writer(SourceId::LOCATION);
        writer
            .write(
                LOCATION,
                ObservationValue::Location(GeoPoint {
                    latitude: 1.0,
                    longitude: 2.0,
                    altitude_m: None,
                }),
                at,
            )
            .unwrap();
        writer.finish(Ok(()));
        snapshot
    }

    fn profile(build: impl FnOnce(&mut TelemetryConfig<3, 2>)) -> crate::profile::PacketProfile<2> {
        let mut config = TelemetryConfig::<3, 2>::default();
        config.add_profile("p", PacketFormatId::Robonomics).unwrap();
        build(&mut config);
        config.get("p").unwrap().clone()
    }

    fn names<const N: usize>(view: &SectionView<'_, N>) -> std::vec::Vec<u8> {
        view.observations().map(|o| o.measurement.0).collect()
    }

    #[test]
    fn different_profiles_make_different_sections_from_one_snapshot() {
        let snapshot = snapshot();
        let community = profile(|c| {
            c.set_public(
                "p",
                DataSelection::local(Selection::NONE.with(BMX280_TEMPERATURE)),
                &Caps,
            )
            .unwrap()
        });
        let owner = profile(|c| {
            c.set_public(
                "p",
                DataSelection::local(Selection::NONE.with(SDS011_PM10)),
                &Caps,
            )
            .unwrap();
            c.add_private(
                "p",
                RecipientAddress([1; 32]),
                DataSelection::local(Selection::NONE.with(LOCATION)),
                &Caps,
            )
            .unwrap();
        });
        let a = prepare(&snapshot, &community, &all_cataloged()).unwrap();
        let b = prepare(&snapshot, &owner, &all_cataloged()).unwrap();
        assert_eq!(names(&a.public), [1]);
        assert!(a.private.is_empty());
        assert_eq!(names(&b.public), [8]);
        assert_eq!(names(&b.private[0].view), [0]);
        assert_ne!(a.profile, b.profile);
        // The views borrow the one snapshot: the same recorded value.
        assert!(core::ptr::eq(
            a.public.observations().next().unwrap(),
            &snapshot.observations[0]
        ));
    }

    #[test]
    fn all_does_not_include_location_and_nothing_selected_is_no_packet() {
        let snapshot = snapshot();
        let all = profile(|c| {
            c.set_public("p", DataSelection::local(Selection::ALL), &Caps)
                .unwrap()
        });
        let sections = prepare(&snapshot, &all, &all_cataloged()).unwrap();
        assert_eq!(names(&sections.public), [1, 8]);

        let empty = profile(|_| {});
        assert_eq!(
            prepare(&snapshot, &empty, &all_cataloged()).unwrap_err(),
            PrepareError::NothingToPackage
        );
    }

    #[test]
    fn a_missing_selected_measurement_is_omitted_and_location_only_is_a_packet() {
        let mut snapshot = TelemetrySnapshot::<4>::new(MonotonicTimestamp::from_millis(0));
        let mut writer = snapshot.writer(SourceId::LOCATION);
        writer
            .write(
                LOCATION,
                ObservationValue::Location(GeoPoint {
                    latitude: 1.0,
                    longitude: 2.0,
                    altitude_m: Some(3.0),
                }),
                MonotonicTimestamp::from_millis(0),
            )
            .unwrap();
        writer.finish(Ok(()));
        let owner = profile(|c| {
            c.add_private(
                "p",
                RecipientAddress([1; 32]),
                DataSelection::local(Selection::NONE.with(LOCATION).with(SDS011_PM10)),
                &Caps,
            )
            .unwrap()
        });
        let sections = prepare(&snapshot, &owner, &all_cataloged()).unwrap();
        assert!(sections.public.is_empty());
        assert_eq!(names(&sections.private[0].view), [0]);
    }
}
