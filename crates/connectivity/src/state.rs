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
//! The effective routes and the runtime status, shared between the service
//! (Actions, status) and the publisher task.

use core::cell::{Cell, RefCell};

use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::signal::Signal;
use heapless::Vec;

use crate::config::{MAX_ROUTES, RouteSet};
use crate::router::RouteView;

/// What the publisher reports about the last time it looked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Health {
    /// Packet production failed the last time (the reason code).
    pub last_build_failure: Option<&'static str>,
    /// Captures taken.
    pub captures: u32,
    /// Packets built.
    pub built: u32,
}

/// Effective routes and runtime status.
///
/// The configuration owner publishes the committed routes here in the commit
/// step ([`publish`](Self::publish)); the publisher reads them, waits for
/// changes with [`changed`](Self::changed) and reports route views and
/// health back for the status of the service.
pub struct ConnectivityState<M: RawMutex> {
    routes: BlockingMutex<M, RefCell<RouteSet>>,
    views: BlockingMutex<M, RefCell<Vec<RouteView, MAX_ROUTES>>>,
    health: BlockingMutex<M, RefCell<Health>>,
    unreadable: BlockingMutex<M, Cell<bool>>,
    changed: Signal<M, u32>,
}

impl<M: RawMutex> Default for ConnectivityState<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: RawMutex> ConnectivityState<M> {
    /// No route: nothing is sent.
    pub const fn new() -> Self {
        Self {
            routes: BlockingMutex::new(RefCell::new(RouteSet {
                generation: 0,
                routes: Vec::new(),
            })),
            views: BlockingMutex::new(RefCell::new(Vec::new())),
            health: BlockingMutex::new(RefCell::new(Health {
                last_build_failure: None,
                captures: 0,
                built: 0,
            })),
            unreadable: BlockingMutex::new(Cell::new(false)),
            changed: Signal::new(),
        }
    }

    /// Makes `routes` effective and wakes the publisher.
    pub fn publish(&self, routes: &RouteSet) {
        self.unreadable.lock(|flag| flag.set(false));
        let generation = routes.generation;
        self.routes
            .lock(|current| *current.borrow_mut() = routes.clone());
        self.changed.signal(generation);
    }

    /// The stored routes could not be read: no route is active and the status
    /// says so.
    pub fn mark_unreadable(&self) {
        self.unreadable.lock(|flag| flag.set(true));
        self.publish_empty();
    }

    fn publish_empty(&self) {
        let generation = self.generation();
        self.routes.lock(|current| {
            *current.borrow_mut() = RouteSet {
                generation,
                routes: Vec::new(),
            };
        });
        self.changed.signal(generation);
    }

    /// Whether the stored routes could not be read.
    pub fn is_unreadable(&self) -> bool {
        self.unreadable.lock(Cell::get)
    }

    /// The generation of the effective routes.
    pub fn generation(&self) -> u32 {
        self.routes.lock(|routes| routes.borrow().generation)
    }

    /// A copy of the effective routes.
    pub fn routes(&self) -> RouteSet {
        self.routes.lock(|routes| routes.borrow().clone())
    }

    /// Waits for the next publication of routes.
    pub async fn changed(&self) -> u32 {
        self.changed.wait().await
    }

    /// Stores the route views and health of the publisher.
    pub fn report(&self, views: Vec<RouteView, MAX_ROUTES>, health: Health) {
        self.views.lock(|current| *current.borrow_mut() = views);
        self.health.lock(|current| *current.borrow_mut() = health);
    }

    /// The last reported route views.
    pub fn views(&self) -> Vec<RouteView, MAX_ROUTES> {
        self.views.lock(|views| views.borrow().clone())
    }

    /// The last reported health.
    pub fn health(&self) -> Health {
        self.health.lock(|health| health.borrow().clone())
    }
}
