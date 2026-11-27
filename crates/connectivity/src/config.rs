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
//! Persistent routing configuration.
//!
//! The model is **fixed and index-addressed**: [`Routes`] holds `ROUTES`
//! route **slots** (Altruist Urban: 2), addressed by their index (`0`, `1`),
//! each empty or holding one [`Route`]. An index is a stable address: deleting
//! the route of one slot never moves another. There are no names.
//!
//! A route publishes one telemetry profile slot (`profile_index`) on its own
//! interval to a primary endpoint (an opaque URI, see [`crate::endpoint`]) with **at most one fallback**
//! endpoint; the fallback is another way to deliver the *same* packet, never a
//! separate publisher, profile or interval.
//!
//! * The default is **no route**: nothing is sent until a route is set
//!   explicitly, an endpoint is never inferred.
//! * [`set_endpoint`](Routes::set_endpoint) creates a route (interval
//!   [`DEFAULT_INTERVAL_S`], no fallback), replaces the profile and primary
//!   endpoint of an existing one (interval and fallback stay) or, when the
//!   endpoint is absent, deletes it.
//! * A new or replaced route is a new **generation**: it inherits no queue,
//!   backoff, counter or packet of whatever the slot held before. Only the
//!   interval and the fallback can be edited without that.
//!
//! The whole configuration is one typed bounded value, kept by the
//! firmware's configuration owner as the private section
//! [`ConnectivitySection`] and edited atomically through the Actions of the
//! `connectivity` service. Every mutation here works on a value, so a
//! candidate can be validated and published only after a successful commit.
//! There is no migration: the sections of earlier versions (named routes) are
//! not read; a stored section that is obsolete or corrupt is reported and
//! leaves no active route.

use minicbor::{CborLen, Decode, Encode};
use rohi_config::ConfigSection;

use crate::endpoint::{EndpointError, EndpointPolicy, EndpointUri};

/// Shortest allowed publication interval, in seconds.
pub const MIN_INTERVAL_S: u32 = 10;
/// Longest allowed publication interval, in seconds.
pub const MAX_INTERVAL_S: u32 = 24 * 60 * 60;
/// The interval of a new route, in seconds.
pub const DEFAULT_INTERVAL_S: u32 = 60;

/// A route.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode, CborLen)]
pub struct Route {
    /// The telemetry profile slot it publishes.
    #[n(0)]
    pub profile_index: u8,
    /// Seconds between publications.
    #[n(1)]
    pub interval_s: u32,
    /// The primary endpoint.
    #[n(2)]
    pub endpoint: EndpointUri,
    /// The only fallback, tried after the primary failed.
    #[n(3)]
    pub fallback: Option<EndpointUri>,
    /// The generation at which the route was created or its profile or
    /// primary endpoint replaced: runtime state of an earlier route of the
    /// slot is never inherited.
    #[n(4)]
    pub generation: u32,
    /// The generation of the last change of the destination set (a
    /// replacement or a fallback change): queued packets of an older one are
    /// dropped.
    #[n(5)]
    pub destinations: u32,
}

/// Why a mutation was refused. A refused mutation changes nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteError {
    /// The route index is not a slot of this build.
    InvalidIndex,
    /// The slot holds no route.
    RouteNotFound,
    /// The endpoint is already used by this route, as primary or fallback.
    DuplicateEndpoint,
    /// The endpoint is not a valid URI of its scheme.
    InvalidEndpoint,
    /// This build has no backend for the scheme of the endpoint.
    UnsupportedEndpoint,
    /// The interval is outside [`MIN_INTERVAL_S`]..=[`MAX_INTERVAL_S`].
    InvalidInterval,
    /// A stored value is inconsistent (generations).
    InconsistentGeneration,
}

/// All route slots.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode, CborLen)]
pub struct Routes<const ROUTES: usize> {
    /// Monotonic counter: every successful mutation increments it.
    #[n(0)]
    pub generation: u32,
    /// The slots, by route index. A stored array of another length does not
    /// decode.
    #[n(1)]
    pub slots: [Option<Route>; ROUTES],
}

impl<const R: usize> Default for Routes<R> {
    /// No route: nothing is sent.
    fn default() -> Self {
        Self::EMPTY
    }
}

fn same_endpoint(policy: &impl EndpointPolicy, a: &EndpointUri, b: &EndpointUri) -> bool {
    a == b || policy.equivalent(a.as_str(), b.as_str())
}

fn check_endpoint(policy: &impl EndpointPolicy, uri: &EndpointUri) -> Result<(), RouteError> {
    policy.validate(uri.as_str()).map_err(|error| match error {
        EndpointError::Invalid => RouteError::InvalidEndpoint,
        EndpointError::UnsupportedScheme => RouteError::UnsupportedEndpoint,
    })
}

impl<const R: usize> Routes<R> {
    /// No route in any slot.
    pub const EMPTY: Self = Self {
        generation: 0,
        slots: [const { None }; R],
    };

    /// The route in slot `index`; `None` for an empty slot or an index
    /// outside the slots.
    pub fn get(&self, index: usize) -> Option<&Route> {
        self.slots.get(index)?.as_ref()
    }

    /// The number of slots that hold a route.
    pub fn count(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_some()).count()
    }

    fn bump(&mut self) -> u32 {
        self.generation = self.generation.saturating_add(1);
        self.generation
    }

    fn check(index: usize) -> Result<(), RouteError> {
        if index < R {
            Ok(())
        } else {
            Err(RouteError::InvalidIndex)
        }
    }

    /// Creates, replaces or deletes the route of slot `index`.
    ///
    /// * With an `endpoint`, an empty slot gets a route for the telemetry
    ///   profile slot `profile_index` with [`DEFAULT_INTERVAL_S`] and no
    ///   fallback. A route that exists keeps its interval and fallback and
    ///   gets the new profile and primary endpoint, which is a new
    ///   generation: its queue, backoff and counters are not inherited. The
    ///   same profile and endpoint again change nothing. The caller checked
    ///   that `profile_index` names an existing telemetry profile.
    /// * With **no** endpoint the route is deleted (`profile_index` is not
    ///   looked at); an empty slot stays empty.
    ///
    /// The endpoint is checked by `policy` (supported scheme, valid for it, not
    /// equivalent to the fallback) before anything changes.
    ///
    /// ```
    /// use rohi_connectivity::{DEFAULT_INTERVAL_S, Routes, StructuralPolicy};
    ///
    /// let mut routes = Routes::<2>::default();
    /// routes.set_endpoint(1, 0, Some("http://ingress.example"), &StructuralPolicy)?;
    /// assert!(routes.get(0).is_none(), "slot 0 stays empty");
    /// assert_eq!(routes.get(1).map(|r| r.interval_s), Some(DEFAULT_INTERVAL_S));
    ///
    /// routes.set_endpoint(1, 0, None, &StructuralPolicy)?; // an absent endpoint deletes the route
    /// assert_eq!(routes.count(), 0);
    /// # Ok::<(), rohi_connectivity::RouteError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// [`RouteError`]; the value is unchanged.
    pub fn set_endpoint(
        &mut self,
        index: usize,
        profile_index: u8,
        endpoint: Option<&str>,
        policy: &impl EndpointPolicy,
    ) -> Result<(), RouteError> {
        Self::check(index)?;
        let Some(endpoint) = endpoint else {
            if self.slots[index].is_some() {
                self.bump();
                self.slots[index] = None;
            }
            return Ok(());
        };
        let endpoint = EndpointUri::new(endpoint).ok_or(RouteError::InvalidEndpoint)?;
        check_endpoint(policy, &endpoint)?;
        match &self.slots[index] {
            Some(route) if route.profile_index == profile_index && route.endpoint == endpoint => {
                return Ok(());
            }
            Some(route)
                if route
                    .fallback
                    .as_ref()
                    .is_some_and(|fallback| same_endpoint(policy, fallback, &endpoint)) =>
            {
                return Err(RouteError::DuplicateEndpoint);
            }
            _ => {}
        }
        let generation = self.bump();
        let (interval_s, fallback) = match self.slots[index].take() {
            Some(route) => (route.interval_s, route.fallback),
            None => (DEFAULT_INTERVAL_S, None),
        };
        self.slots[index] = Some(Route {
            profile_index,
            interval_s,
            endpoint,
            fallback,
            generation,
            destinations: generation,
        });
        Ok(())
    }

    /// Changes only the publication interval of the route in slot `index`.
    ///
    /// # Errors
    ///
    /// [`RouteError`]; the value is unchanged.
    pub fn set_interval(&mut self, index: usize, interval_s: u32) -> Result<(), RouteError> {
        Self::check(index)?;
        if !(MIN_INTERVAL_S..=MAX_INTERVAL_S).contains(&interval_s) {
            return Err(RouteError::InvalidInterval);
        }
        if self.slots[index].is_none() {
            return Err(RouteError::RouteNotFound);
        }
        self.bump();
        if let Some(route) = &mut self.slots[index] {
            route.interval_s = interval_s;
        }
        Ok(())
    }

    /// Sets the only fallback of the route in slot `index`, replacing the
    /// earlier one; with **no** endpoint the fallback is removed. The same
    /// fallback again changes nothing.
    ///
    /// # Errors
    ///
    /// [`RouteError`]; the value is unchanged.
    pub fn set_fallback(
        &mut self,
        index: usize,
        endpoint: Option<&str>,
        policy: &impl EndpointPolicy,
    ) -> Result<(), RouteError> {
        Self::check(index)?;
        let uri = match endpoint {
            Some(endpoint) => {
                let uri = EndpointUri::new(endpoint).ok_or(RouteError::InvalidEndpoint)?;
                check_endpoint(policy, &uri)?;
                Some(uri)
            }
            None => None,
        };
        let route = self.slots[index]
            .as_ref()
            .ok_or(RouteError::RouteNotFound)?;
        if route.fallback == uri {
            return Ok(());
        }
        if uri
            .as_ref()
            .is_some_and(|fallback| same_endpoint(policy, &route.endpoint, fallback))
        {
            return Err(RouteError::DuplicateEndpoint);
        }
        let generation = self.bump();
        if let Some(route) = &mut self.slots[index] {
            route.fallback = uri;
            route.destinations = generation;
        }
        Ok(())
    }

    /// Checks a value decoded from storage: endpoints valid and distinct
    /// within a route, intervals in range, generations consistent. A value
    /// that fails is never used. The telemetry profile of a route is checked
    /// by the firmware, which knows how many profile slots it has.
    ///
    /// # Errors
    ///
    /// The first [`RouteError`] found.
    pub fn validate(&self, policy: &impl EndpointPolicy) -> Result<(), RouteError> {
        for route in self.slots.iter().flatten() {
            if !(MIN_INTERVAL_S..=MAX_INTERVAL_S).contains(&route.interval_s) {
                return Err(RouteError::InvalidInterval);
            }
            if !route.endpoint.is_well_formed() {
                return Err(RouteError::InvalidEndpoint);
            }
            check_endpoint(policy, &route.endpoint)?;
            if route.generation == 0
                || route.generation > route.destinations
                || route.destinations > self.generation
            {
                return Err(RouteError::InconsistentGeneration);
            }
            if let Some(fallback) = &route.fallback {
                if !fallback.is_well_formed() {
                    return Err(RouteError::InvalidEndpoint);
                }
                check_endpoint(policy, fallback)?;
                if same_endpoint(policy, &route.endpoint, fallback) {
                    return Err(RouteError::DuplicateEndpoint);
                }
            }
        }
        Ok(())
    }
}

/// The persisted section: all route slots of a device that keeps `ROUTES` of them (a const
/// parameter chosen by the firmware; Altruist Urban: 2). The capacity is the type, so it is also
/// the storage: a stored section of another size does not decode and is reported like any
/// unreadable section, which leaves no active route. Nothing is truncated.
///
/// Version 2 is the indexed-slot model; the named routes of version 1 are not read.
#[derive(Clone, Debug, PartialEq, Eq, Default, Encode, Decode, CborLen, ConfigSection)]
#[cbor(transparent)]
#[config(id = 0x4d75_b590_2252_1f7d, version = 2)]
pub struct ConnectivitySection<const ROUTES: usize>(#[n(0)] pub Routes<ROUTES>);

#[cfg(test)]
mod tests;
