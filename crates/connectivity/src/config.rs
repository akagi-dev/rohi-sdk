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
//! A **route** publishes one telemetry packet profile on its own interval to
//! a primary HTTP endpoint with an ordered list of fallback endpoints; a
//! fallback is another way to deliver the *same* packet, never a separate
//! publisher, profile or interval.
//!
//! * The default is **zero routes**: nothing is sent until a route is added
//!   explicitly, an endpoint is never inferred.
//! * `id`, `profile` and `primary` of a route are immutable: they change by
//!   removing and adding the route again, which is a new generation that
//!   inherits no queue, endpoint state or backoff.
//! * Only the interval can be edited in place; fallbacks are appended
//!   (priority is their order) and removed.
//!
//! The whole configuration is one typed bounded value, kept by the
//! firmware's configuration owner as the private section
//! [`ConnectivitySection`] and edited atomically through the Actions of the
//! `connectivity` service. Every mutation here works on a value, so a
//! candidate can be validated and published only after a successful commit.
//! There is no migration: the earlier single-endpoint `connectivity` section
//! is not read; a stored section that is obsolete or corrupt is reported and
//! leaves no active route.

use heapless::{String, Vec};
use minicbor::{CborLen, Decode, Encode};
use rohi_config::ConfigSection;
use rohi_telemetry::PacketProfileId;

use crate::endpoint::EndpointUri;

/// Maximum number of routes, a budget goal checked against the 1 KiB section
/// limit by tests.
pub const MAX_ROUTES: usize = 3;
/// Maximum number of fallbacks of a route.
pub const MAX_FALLBACKS: usize = 2;
/// Maximum length of a route or fallback id, in bytes.
pub const ID_LEN: usize = 16;
/// Shortest allowed publication interval, in seconds.
pub const MIN_INTERVAL_S: u32 = 10;
/// Longest allowed publication interval, in seconds.
pub const MAX_INTERVAL_S: u32 = 24 * 60 * 60;
/// The interval of a new route, in seconds.
pub const DEFAULT_INTERVAL_S: u32 = 60;

macro_rules! name {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, Hash, Encode, Decode, CborLen)]
        #[cbor(transparent)]
        pub struct $name(
            #[n(0)]
            #[cbor(
                encode_with = "rohi_config::cbor::text::encode",
                decode_with = "rohi_config::cbor::text::decode",
                cbor_len = "rohi_config::cbor::text::cbor_len"
            )]
            String<ID_LEN>,
        );

        impl $name {
            /// `None` unless `id` is 1 to [`ID_LEN`] bytes of `a-z`, `0-9`,
            /// `-` and `_`.
            pub fn new(id: &str) -> Option<Self> {
                let valid = !id.is_empty()
                    && id.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'
                    });
                if !valid {
                    return None;
                }
                String::try_from(id).ok().map(Self)
            }

            /// The id as text.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl core::fmt::Debug for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                core::fmt::Debug::fmt(self.as_str(), f)
            }
        }
    };
}

name!(
    /// The stable text id of a route.
    RouteId
);
name!(
    /// The id of a fallback, unique within its route.
    FallbackId
);

/// The telemetry profile a route publishes, as it was when the route was
/// created. A deleted and recreated profile has another incarnation and is
/// never silently picked up by an old route.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode, CborLen)]
pub struct ProfileBinding {
    /// The profile id.
    #[n(0)]
    pub id: PacketProfileId,
    /// The incarnation of the profile at binding time.
    #[n(1)]
    pub incarnation: u32,
}

/// One alternative delivery path of a route.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode, CborLen)]
pub struct Fallback {
    /// The id, unique within the route.
    #[n(0)]
    pub id: FallbackId,
    /// Where to send the packet.
    #[n(1)]
    pub endpoint: EndpointUri,
}

/// A route.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode, CborLen)]
pub struct Route<const FALLBACKS: usize> {
    /// The id, immutable.
    #[n(0)]
    pub id: RouteId,
    /// The published profile, immutable.
    #[n(1)]
    pub profile: ProfileBinding,
    /// Seconds between publications.
    #[n(2)]
    pub interval_s: u32,
    /// The primary endpoint, immutable.
    #[n(3)]
    pub primary: EndpointUri,
    /// The fallbacks in priority order.
    #[n(4)]
    #[cbor(
        encode_with = "rohi_config::cbor::seq::encode",
        decode_with = "rohi_config::cbor::seq::decode",
        cbor_len = "rohi_config::cbor::seq::cbor_len"
    )]
    pub fallbacks: Vec<Fallback, FALLBACKS>,
    /// The generation at which the route was created: runtime state of an
    /// earlier route with the same id is never inherited.
    #[n(5)]
    pub generation: u32,
    /// The generation of the last change of the destination set (a fallback
    /// added or removed): queued packets of an older one are dropped.
    #[n(6)]
    pub destinations: u32,
}

/// Why a mutation was refused. A refused mutation changes nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteError {
    /// A route or fallback id is not valid.
    InvalidId,
    /// A route with this id exists.
    DuplicateRoute,
    /// No route with this id.
    RouteNotFound,
    /// The route already has a fallback with this id.
    DuplicateFallback,
    /// No such fallback.
    FallbackNotFound,
    /// The endpoint is already used by this route, as primary or fallback.
    DuplicateEndpoint,
    /// The endpoint is not a valid `http://host[:port]`.
    InvalidEndpoint,
    /// The interval is outside [`MIN_INTERVAL_S`]..=[`MAX_INTERVAL_S`].
    InvalidInterval,
    /// The route table is full.
    TooManyRoutes,
    /// The route has all its fallbacks.
    TooManyFallbacks,
}

/// All routes.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode, CborLen)]
pub struct Routes<const ROUTES: usize, const FALLBACKS: usize> {
    /// Monotonic counter: every successful mutation increments it.
    #[n(0)]
    pub generation: u32,
    /// The routes, in creation order.
    #[n(1)]
    #[cbor(
        encode_with = "rohi_config::cbor::seq::encode",
        decode_with = "rohi_config::cbor::seq::decode",
        cbor_len = "rohi_config::cbor::seq::cbor_len"
    )]
    pub routes: Vec<Route<FALLBACKS>, ROUTES>,
}

impl<const R: usize, const F: usize> Default for Routes<R, F> {
    /// No route: nothing is sent.
    fn default() -> Self {
        Self {
            generation: 0,
            routes: Vec::new(),
        }
    }
}

fn same_endpoint(a: &EndpointUri, b: &EndpointUri) -> bool {
    match (a.endpoint(), b.endpoint()) {
        (Some(a), Some(b)) => a.host.eq_ignore_ascii_case(b.host) && a.port == b.port,
        _ => a == b,
    }
}

impl<const R: usize, const F: usize> Routes<R, F> {
    /// The route `id`.
    pub fn get(&self, id: &str) -> Option<&Route<F>> {
        self.routes.iter().find(|route| route.id.as_str() == id)
    }

    fn index(&self, id: &str) -> Result<usize, RouteError> {
        self.routes
            .iter()
            .position(|route| route.id.as_str() == id)
            .ok_or(RouteError::RouteNotFound)
    }

    fn bump(&mut self) -> u32 {
        self.generation = self.generation.saturating_add(1);
        self.generation
    }

    /// Adds a route with the default interval and no fallback. The caller
    /// resolved `profile` against the existing telemetry profiles.
    ///
    /// # Errors
    ///
    /// [`RouteError`]; the value is unchanged.
    pub fn add_route(
        &mut self,
        id: &str,
        profile: ProfileBinding,
        primary: &str,
    ) -> Result<(), RouteError> {
        let id = RouteId::new(id).ok_or(RouteError::InvalidId)?;
        let primary = EndpointUri::new(primary).ok_or(RouteError::InvalidEndpoint)?;
        if self.get(id.as_str()).is_some() {
            return Err(RouteError::DuplicateRoute);
        }
        if self.routes.is_full() {
            return Err(RouteError::TooManyRoutes);
        }
        let generation = self.bump();
        self.routes
            .push(Route {
                id,
                profile,
                interval_s: DEFAULT_INTERVAL_S,
                primary,
                fallbacks: Vec::new(),
                generation,
                destinations: generation,
            })
            .map_err(|_| RouteError::TooManyRoutes)
    }

    /// Removes a route with its fallbacks.
    ///
    /// # Errors
    ///
    /// [`RouteError::RouteNotFound`].
    pub fn remove_route(&mut self, id: &str) -> Result<(), RouteError> {
        let index = self.index(id)?;
        self.routes.remove(index);
        self.bump();
        Ok(())
    }

    /// Changes only the publication interval of a route.
    ///
    /// # Errors
    ///
    /// [`RouteError`].
    pub fn set_interval(&mut self, id: &str, interval_s: u32) -> Result<(), RouteError> {
        if !(MIN_INTERVAL_S..=MAX_INTERVAL_S).contains(&interval_s) {
            return Err(RouteError::InvalidInterval);
        }
        let index = self.index(id)?;
        self.bump();
        self.routes[index].interval_s = interval_s;
        Ok(())
    }

    /// Appends a fallback; its position is its priority.
    ///
    /// # Errors
    ///
    /// [`RouteError`]; the value is unchanged.
    pub fn add_fallback(
        &mut self,
        route: &str,
        fallback: &str,
        endpoint: &str,
    ) -> Result<(), RouteError> {
        let id = FallbackId::new(fallback).ok_or(RouteError::InvalidId)?;
        let endpoint = EndpointUri::new(endpoint).ok_or(RouteError::InvalidEndpoint)?;
        let index = self.index(route)?;
        let existing = &self.routes[index];
        if existing.fallbacks.iter().any(|f| f.id == id) {
            return Err(RouteError::DuplicateFallback);
        }
        if same_endpoint(&existing.primary, &endpoint)
            || existing
                .fallbacks
                .iter()
                .any(|f| same_endpoint(&f.endpoint, &endpoint))
        {
            return Err(RouteError::DuplicateEndpoint);
        }
        if existing.fallbacks.is_full() {
            return Err(RouteError::TooManyFallbacks);
        }
        let generation = self.bump();
        let route = &mut self.routes[index];
        route
            .fallbacks
            .push(Fallback { id, endpoint })
            .map_err(|_| RouteError::TooManyFallbacks)?;
        route.destinations = generation;
        Ok(())
    }

    /// Removes a fallback.
    ///
    /// # Errors
    ///
    /// [`RouteError`].
    pub fn remove_fallback(&mut self, route: &str, fallback: &str) -> Result<(), RouteError> {
        let index = self.index(route)?;
        let position = self.routes[index]
            .fallbacks
            .iter()
            .position(|f| f.id.as_str() == fallback)
            .ok_or(RouteError::FallbackNotFound)?;
        let generation = self.bump();
        let route = &mut self.routes[index];
        route.fallbacks.remove(position);
        route.destinations = generation;
        Ok(())
    }

    /// Checks a value decoded from storage: ids valid and unique, endpoints
    /// valid and distinct within a route, intervals in range, generations
    /// consistent. A value that fails is never used.
    ///
    /// # Errors
    ///
    /// The first [`RouteError`] found.
    pub fn validate(&self) -> Result<(), RouteError> {
        for (index, route) in self.routes.iter().enumerate() {
            if RouteId::new(route.id.as_str()).is_none() {
                return Err(RouteError::InvalidId);
            }
            if self.routes[..index].iter().any(|r| r.id == route.id) {
                return Err(RouteError::DuplicateRoute);
            }
            if !(MIN_INTERVAL_S..=MAX_INTERVAL_S).contains(&route.interval_s) {
                return Err(RouteError::InvalidInterval);
            }
            if route.primary.endpoint().is_none() {
                return Err(RouteError::InvalidEndpoint);
            }
            if route.generation > self.generation || route.destinations > self.generation {
                return Err(RouteError::RouteNotFound);
            }
            for (position, fallback) in route.fallbacks.iter().enumerate() {
                if FallbackId::new(fallback.id.as_str()).is_none() {
                    return Err(RouteError::InvalidId);
                }
                if fallback.endpoint.endpoint().is_none() {
                    return Err(RouteError::InvalidEndpoint);
                }
                if route.fallbacks[..position]
                    .iter()
                    .any(|f| f.id == fallback.id)
                {
                    return Err(RouteError::DuplicateFallback);
                }
                if same_endpoint(&route.primary, &fallback.endpoint)
                    || route.fallbacks[..position]
                        .iter()
                        .any(|f| same_endpoint(&f.endpoint, &fallback.endpoint))
                {
                    return Err(RouteError::DuplicateEndpoint);
                }
            }
        }
        Ok(())
    }
}

/// The routes this device keeps.
pub type RouteSet = Routes<MAX_ROUTES, MAX_FALLBACKS>;

/// The persisted section: all routes. A stored section that is obsolete or
/// unreadable is reported and leaves no active route.
#[derive(Clone, Debug, PartialEq, Eq, Default, Encode, Decode, CborLen, ConfigSection)]
#[cbor(transparent)]
#[config(version = 1)]
pub struct ConnectivitySection(#[n(0)] pub RouteSet);

#[cfg(test)]
mod tests;
