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

//! Revisions follow completed commits, not waiting callers.
//!
//! The store below models the configuration owner of the firmware: a request
//! is queued, the owner task commits, publishes and advances the revision in
//! one synchronous step, and answers. A caller that is cancelled after its
//! request was queued still gets its commit, and the revision must say so.

extern crate std;

use core::cell::{Cell, RefCell};
use core::future::poll_fn;
use core::pin::pin;
use core::task::Poll;
use std::vec::Vec;

use embassy_futures::poll_once;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use rohi_config::{Override, Value};
use rohi_service::{
    Config, ConfigError, ConfigSectionOps, ConfigUpdate, Mutation, MutationOp, Revision, Section,
    SectionStore, StoreError,
};

#[derive(Clone, Debug, Default, PartialEq, Config)]
#[config(name = "lamp", overrides = LampOverrides, version = 1)]
struct Lamp {
    #[config(id = 1, min = 0, max = 100)]
    level: u8,
}

#[derive(Clone)]
enum Request {
    Store(LampOverrides),
    Remove,
}

/// The commit owner: a queue, the persisted overrides and the revision.
#[derive(Default)]
struct Owner {
    queue: RefCell<Option<Request>>,
    response: Cell<Option<Result<(), StoreError>>>,
    persisted: RefCell<Option<LampOverrides>>,
    published: RefCell<Vec<u8>>,
    revision: Revision<NoopRawMutex>,
    fail_next: Cell<bool>,
}

impl Owner {
    /// The owner task handles the queued request, if there is one.
    fn process(&self) {
        let Some(request) = self.queue.take() else {
            return;
        };
        if self.fail_next.take() {
            self.response.set(Some(Err(StoreError::Unavailable)));
            return;
        }
        match request {
            Request::Store(overrides) => {
                let effective = rohi_config::materialize::<Lamp>(&Default::default(), &overrides);
                *self.persisted.borrow_mut() = Some(overrides);
                self.published.borrow_mut().push(effective.level);
            }
            Request::Remove => {
                *self.persisted.borrow_mut() = None;
                self.published.borrow_mut().push(Lamp::default().level);
            }
        }
        // Commit, publication and revision in one step, before the answer.
        self.revision.bump();
        self.response.set(Some(Ok(())));
    }

    /// A factory reset: the whole storage is formatted, every section counts.
    fn factory_reset(&self) {
        *self.persisted.borrow_mut() = None;
        self.revision.bump();
    }

    async fn call(&self, request: Request) -> Result<(), StoreError> {
        // The answer to an abandoned call is never taken for this one (the
        // firmware numbers its requests).
        self.response.set(None);
        *self.queue.borrow_mut() = Some(request);
        poll_fn(|_| match self.response.take() {
            Some(result) => Poll::Ready(result),
            None => Poll::Pending,
        })
        .await
    }
}

impl SectionStore<Lamp> for &Owner {
    fn firmware_overrides(&self) -> LampOverrides {
        LampOverrides::default()
    }

    async fn load(&self) -> Result<Option<LampOverrides>, StoreError> {
        Ok(self.persisted.borrow().clone())
    }

    async fn store(&self, overrides: &LampOverrides, _: &Lamp) -> Result<(), StoreError> {
        self.call(Request::Store(overrides.clone())).await
    }

    async fn remove(&self, _: &Lamp) -> Result<(), StoreError> {
        self.call(Request::Remove).await
    }

    fn revision(&self) -> Option<u32> {
        Some(self.revision.get())
    }
}

type LampSection<'a> = Section<Lamp, &'a Owner, NoopRawMutex>;

fn set(level: u64) -> [Mutation; 1] {
    [Mutation {
        field_id: 1,
        op: MutationOp::Set(Value::U64(level)),
    }]
}

fn revision(section: &LampSection<'_>) -> u32 {
    section.info().revision
}

/// Polls `operation` until the owner has answered, running the owner task
/// whenever the caller waits.
fn drive<T>(owner: &Owner, operation: impl core::future::Future<Output = T>) -> T {
    let mut operation = pin!(operation);
    loop {
        if let Poll::Ready(value) = poll_once(operation.as_mut()) {
            return value;
        }
        owner.process();
    }
}

#[test]
fn a_cancelled_patch_still_advances_the_revision() {
    let owner = Owner::default();
    let section = LampSection::new(&owner);
    assert_eq!(revision(&section), 0);

    // The caller queues the request and is cancelled while it waits.
    {
        let mutations = set(30);
        let mut patch = pin!(section.patch(&mutations, Some(0), false));
        assert!(poll_once(patch.as_mut()).is_pending());
        assert!(owner.queue.borrow().is_some());
    }

    // The owner task commits and publishes without the caller.
    owner.process();
    assert_eq!(*owner.published.borrow(), [30]);
    assert!(owner.persisted.borrow().is_some());
    assert_eq!(revision(&section), 1);

    // A client that still holds the previous revision is refused, and nothing
    // is overwritten.
    assert_eq!(
        drive(&owner, section.patch(&set(80), Some(0), false)),
        Err(ConfigError::Conflict)
    );
    assert_eq!(*owner.published.borrow(), [30]);
    // The current revision is accepted.
    let done = drive(&owner, section.patch(&set(80), Some(1), false)).unwrap();
    assert_eq!(done.revision, 2);
    assert_eq!(revision(&section), 2);
}

#[test]
fn a_write_that_never_reached_the_owner_does_not_advance_the_revision() {
    let owner = Owner::default();
    let section = LampSection::new(&owner);
    // Dropped before it is polled at all.
    drop(section.patch(&set(30), None, false));
    // Cancelled while it waits for the section lock: the request is not
    // queued.
    {
        let (a, b) = (set(10), set(20));
        let mut first = pin!(section.patch(&a, None, false));
        assert!(poll_once(first.as_mut()).is_pending());
        owner.queue.take();
        let mut second = pin!(section.patch(&b, None, false));
        assert!(poll_once(second.as_mut()).is_pending());
        assert!(owner.queue.borrow().is_none());
    }
    assert_eq!(revision(&section), 0);
    assert!(owner.persisted.borrow().is_none());
    // The section is usable and counts from where the owner is.
    let done = drive(&owner, section.patch(&set(40), Some(0), false)).unwrap();
    assert_eq!(done.revision, 1);
}

#[test]
fn a_failed_commit_does_not_advance_the_revision() {
    let owner = Owner::default();
    let section = LampSection::new(&owner);
    owner.fail_next.set(true);
    assert_eq!(
        drive(&owner, section.patch(&set(30), None, false)),
        Err(ConfigError::Unavailable)
    );
    assert_eq!(revision(&section), 0);
    assert!(owner.published.borrow().is_empty());
    // Invalid input never reaches the owner.
    assert!(matches!(
        drive(&owner, section.patch(&set(300), None, false)),
        Err(ConfigError::Field(_))
    ));
    assert_eq!(revision(&section), 0);
    // A dry run changes nothing either.
    drive(&owner, section.patch(&set(30), None, true)).unwrap();
    assert_eq!(revision(&section), 0);
    assert!(owner.queue.borrow().is_none());
}

#[test]
fn a_cancelled_reset_still_advances_the_revision() {
    let owner = Owner::default();
    let section = LampSection::new(&owner);
    drive(&owner, section.patch(&set(30), None, false)).unwrap();
    {
        let reset = section.reset(Some(1), false);
        let mut reset = pin!(reset);
        assert!(poll_once(reset.as_mut()).is_pending());
    }
    owner.process();
    assert!(owner.persisted.borrow().is_none());
    assert_eq!(revision(&section), 2);
    assert_eq!(
        drive(&owner, section.reset(Some(1), false)),
        Err(ConfigError::Conflict)
    );
}

#[test]
fn a_cancelled_service_update_still_advances_the_revision() {
    let owner = Owner::default();
    let section = LampSection::new(&owner);
    {
        let update = section.update(|overrides| overrides.level = Override::Value(55));
        let mut update = pin!(update);
        assert!(poll_once(update.as_mut()).is_pending());
    }
    owner.process();
    assert_eq!(revision(&section), 1);
    // The client and the service share the revision.
    assert_eq!(
        drive(&owner, section.patch(&set(1), Some(0), false)),
        Err(ConfigError::Conflict)
    );
}

#[test]
fn a_cancelled_factory_reset_leaves_no_stale_revision() {
    let owner = Owner::default();
    let section = LampSection::new(&owner);
    drive(&owner, section.patch(&set(30), None, false)).unwrap();
    // The reset is queued and the caller goes away; the owner formats the
    // storage and counts it for every section.
    owner.factory_reset();
    assert_eq!(revision(&section), 2);
    assert_eq!(
        drive(&owner, section.patch(&set(50), Some(1), false)),
        Err(ConfigError::Conflict)
    );
}

#[test]
fn the_revision_counter_counts_and_wraps() {
    let revision = Revision::<NoopRawMutex>::new();
    assert_eq!(revision.get(), 0);
    assert_eq!(revision.bump(), 1);
    assert_eq!(revision.get(), 1);
    assert_eq!(Revision::<NoopRawMutex>::default().get(), 0);
}
