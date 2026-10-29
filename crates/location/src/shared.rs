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

/// Where a [`Location`] came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocationSourceKind {
    /// Entered by the user in the `location` config section.
    Configured,
    /// Measured by a GNSS receiver.
    Gps,
}

/// A position on Earth, WGS84.
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

/// The shared current location. Place it in a `static`.
///
/// The owner of the stored configuration publishes the `location` section
/// here whenever it changes (and once at boot); a GNSS runtime publishes its
/// fix. Reading is cheap and never blocks.
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
    /// An invalid section is treated as removed: a wrong position is worse
    /// than none.
    pub fn publish_configured(&self, config: Option<&LocationConfig>) {
        let configured = config
            .filter(|config| config.is_valid())
            .map(LocationConfig::location);
        self.state.lock(|cell| {
            let mut state = cell.get();
            state.configured = configured;
            cell.set(state);
        });
    }

    /// Publishes the fix of a GNSS receiver. The receiver must publish `None`
    /// as soon as the fix is lost or stale; there is no expiry here.
    ///
    /// Reserved for the GNSS service, which does not exist yet.
    pub fn publish_fix(&self, fix: Option<Location>) {
        self.state.lock(|cell| {
            let mut state = cell.get();
            state.fix = fix.map(|fix| Location {
                source: LocationSourceKind::Gps,
                ..fix
            });
            cell.set(state);
        });
    }

    /// The best location: a live fix, otherwise the configured one.
    pub fn current(&self) -> Option<Location> {
        self.state.lock(|cell| {
            let state = cell.get();
            state.fix.or(state.configured)
        })
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
#[derive(Clone, Copy, Debug, Default)]
pub struct NoLocation;

impl LocationSource for NoLocation {
    fn location(&self) -> Option<Location> {
        None
    }
}
