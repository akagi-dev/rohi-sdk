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
//! The current location and where it comes from.

use core::cell::Cell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::RawMutex;

use crate::LocationConfig;

/// Where a [`Location`] came from. The source decides the priority in
/// [`LocationShared::current`]: a live [`Gps`](Self::Gps) fix beats
/// [`Configured`](Self::Configured).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocationSourceKind {
    /// Entered by the user in the `location` config section.
    Configured,
    /// Measured by a GNSS receiver.
    Gps,
}

/// A position on Earth, WGS84.
///
/// The fields are plain data and are not validated here: user input is
/// validated by [`LocationConfig`], and the telemetry encoder leaves out
/// non-finite values.
///
/// ```
/// use rohi_location::{Location, LocationSourceKind};
///
/// let here = Location {
///     latitude: 52.52,
///     longitude: 13.405,
///     height_m: None,
///     source: LocationSourceKind::Configured,
/// };
/// assert_eq!(here.height_m, None);
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Location {
    /// Latitude in decimal degrees, -90 to 90.
    pub latitude: f64,
    /// Longitude in decimal degrees, -180 to 180.
    pub longitude: f64,
    /// Height above mean sea level in metres, if known.
    pub height_m: Option<f64>,
    /// Who says so.
    pub source: LocationSourceKind,
}

/// Where telemetry and other consumers read the location from.
///
/// Implemented by [`LocationShared`], [`NoLocation`] and by any reference to
/// an implementor, so a consumer can be generic over the provider:
///
/// ```
/// use rohi_location::{LocationSource, NoLocation};
///
/// fn has_position(source: &impl LocationSource) -> bool {
///     source.location().is_some()
/// }
///
/// assert!(!has_position(&NoLocation));
/// ```
pub trait LocationSource {
    /// The location right now, `None` while the device does not know where
    /// it is.
    fn location(&self) -> Option<Location>;
}

impl<T: LocationSource + ?Sized> LocationSource for &T {
    fn location(&self) -> Option<Location> {
        (**self).location()
    }
}

#[derive(Clone, Copy)]
struct State {
    configured: Option<Location>,
    fix: Option<Location>,
}

/// The shared current location. `new` is `const`, so it can live in a
/// `static`.
///
/// The owner of the stored configuration publishes the `location` section
/// here whenever it changes (and once at boot); a GNSS runtime publishes its
/// fix. Reading is cheap and never awaits: each call is one short critical
/// section of the mutex `M` that copies two `Option<Location>` values.
///
/// `M` is the [`RawMutex`] that makes the state shareable between tasks; pick
/// the one that matches who publishes and reads (the Altruist Urban firmware
/// uses a critical-section mutex).
///
/// ```
/// use embassy_sync::blocking_mutex::raw::NoopRawMutex;
/// use rohi_location::{LocationShared, LocationSource};
///
/// let shared = LocationShared::<NoopRawMutex>::default();
/// assert_eq!(shared.location(), None);
/// ```
pub struct LocationShared<M: RawMutex> {
    state: Mutex<M, Cell<State>>,
}

impl<M: RawMutex> LocationShared<M> {
    /// A device that does not know where it is.
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(Cell::new(State {
                configured: None,
                fix: None,
            })),
        }
    }

    /// Publishes the stored `location` section, `None` when it was removed.
    /// An invalid section (see [`LocationConfig::is_valid`]) is treated as
    /// removed: a wrong position is worse than none. This includes the
    /// unconfigured default section, which has no position.
    ///
    /// ```
    /// use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    /// use rohi_location::{LocationConfig, LocationShared};
    ///
    /// let shared = LocationShared::<NoopRawMutex>::new();
    /// let valid = LocationConfig { latitude: 52.52, longitude: 13.405, height_m: None };
    /// shared.publish_configured(Some(&valid));
    /// assert!(shared.current().is_some());
    ///
    /// // An invalid section removes the configured position.
    /// let invalid = LocationConfig { latitude: 91.0, ..valid };
    /// shared.publish_configured(Some(&invalid));
    /// assert!(shared.current().is_none());
    /// ```
    pub fn publish_configured(&self, config: Option<&LocationConfig>) {
        let configured = config
            .filter(|config| config.is_valid())
            .map(LocationConfig::location);
        let previous = self.state.lock(|cell| {
            let mut state = cell.get();
            let previous = state.configured;
            state.configured = configured;
            cell.set(state);
            previous
        });
        let rejected = configured.is_none() && config.is_some_and(|config| !config.is_unset());
        if previous != configured || rejected {
            log_configured(config, configured.is_some());
        }
    }

    /// Publishes the fix of a GNSS receiver. The receiver must publish `None`
    /// as soon as the fix is lost or stale; there is no expiry here. The
    /// `source` of `fix` is always set to [`LocationSourceKind::Gps`]. The
    /// coordinates are not validated.
    ///
    /// Reserved for a GNSS runtime, which does not exist yet.
    pub fn publish_fix(&self, fix: Option<Location>) {
        let had_fix = self.state.lock(|cell| {
            let mut state = cell.get();
            let had_fix = state.fix.is_some();
            state.fix = fix.map(|fix| Location {
                source: LocationSourceKind::Gps,
                ..fix
            });
            cell.set(state);
            had_fix
        });
        match (had_fix, fix.is_some()) {
            (false, true) => log::debug!(target: "location", "GPS fix acquired"),
            (true, false) => log::debug!(target: "location", "GPS fix lost"),
            _ => log::trace!(target: "location", "GPS fix updated"),
        }
    }

    /// The best location: a live fix, otherwise the configured one, otherwise
    /// `None`.
    pub fn current(&self) -> Option<Location> {
        self.state.lock(|cell| {
            let state = cell.get();
            state.fix.or(state.configured)
        })
    }
}

// The coordinates are never logged: the log is persistent and they locate the device.
#[cold]
#[inline(never)]
fn log_configured(config: Option<&LocationConfig>, valid: bool) {
    match config {
        _ if valid => log::debug!(target: "location", "Configured position published"),
        Some(config) if !config.is_unset() => {
            log::warn!(
                target: "location",
                "Configured position is out of range, ignored"
            )
        }
        _ => log::debug!(target: "location", "Configured position cleared"),
    }
}

impl<M: RawMutex> Default for LocationShared<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: RawMutex> LocationSource for LocationShared<M> {
    fn location(&self) -> Option<Location> {
        self.current()
    }
}

/// A source that never knows where the device is, for tests and for
/// firmware without any location.
///
/// ```
/// use rohi_location::{LocationSource, NoLocation};
///
/// assert!(NoLocation.location().is_none());
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct NoLocation;

impl LocationSource for NoLocation {
    fn location(&self) -> Option<Location> {
        None
    }
}
