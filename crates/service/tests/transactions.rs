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

//! Transactions of a section: serialization, commits of a cancelled caller and
//! publication.
//!
//! The store below models the configuration owner of the firmware: a request
//! is queued, the owner task commits and publishes in one synchronous step, and
//! answers. A caller that is cancelled after its request was queued still gets
//! its commit.

extern crate std;

use core::cell::{Cell, RefCell};
use core::future::poll_fn;
use core::pin::pin;
use core::task::Poll;
use std::vec::Vec;

use embassy_futures::join::join;
use embassy_futures::poll_once;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use rohi_config::{Override, Value};
use rohi_service::{
    Config, ConfigError, ConfigSectionOps, ConfigUpdate, Mutation, MutationOp, Section,
    SectionStore, StoreError,
};

#[derive(Clone, Debug, Default, PartialEq, Config)]
#[config(name = "lamp", overrides = LampOverrides, version = 1)]
struct Lamp {
    #[config(id = 1, min = 0, max = 100)]
    level: u8,
    #[config(id = 2, min = 0, max = 255)]
    hue: u8,
}

#[derive(Clone)]
enum Request {
    Store(LampOverrides),
    Remove,
}

/// The commit owner: a queue and the persisted overrides.
#[derive(Default)]
struct Owner {
    queue: RefCell<Option<Request>>,
    response: Cell<Option<Result<(), StoreError>>>,
    persisted: RefCell<Option<LampOverrides>>,
    published: RefCell<Vec<(u8, u8)>>,
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
                self.published
                    .borrow_mut()
                    .push((effective.level, effective.hue));
            }
            Request::Remove => {
                *self.persisted.borrow_mut() = None;
                self.published
                    .borrow_mut()
                    .push((Lamp::default().level, Lamp::default().hue));
            }
        }
        // Commit and publication in one step, before the answer.
        self.response.set(Some(Ok(())));
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
}

type LampSection<'a> = Section<Lamp, &'a Owner, NoopRawMutex>;

fn set(level: u64) -> [Mutation; 1] {
    field(1, level)
}

fn field(field_id: u32, value: u64) -> [Mutation; 1] {
    [Mutation {
        field_id,
        op: MutationOp::Set(Value::U64(value)),
    }]
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
fn a_cancelled_patch_is_still_committed_and_published() {
    let owner = Owner::default();
    let section = LampSection::new(&owner);

    // The caller queues the request and is cancelled while it waits.
    {
        let mutations = set(30);
        let mut patch = pin!(section.patch(&mutations, false));
        assert!(poll_once(patch.as_mut()).is_pending());
        assert!(owner.queue.borrow().is_some());
    }

    // The owner task commits and publishes without the caller.
    owner.process();
    assert_eq!(*owner.published.borrow(), [(30, 0)]);
    assert!(owner.persisted.borrow().is_some());

    // The section is usable afterwards and builds on the committed value.
    drive(&owner, section.patch(&field(2, 7), false)).unwrap();
    assert_eq!(*owner.published.borrow(), [(30, 0), (30, 7)]);
}

#[test]
fn a_write_that_never_reached_the_owner_changes_nothing() {
    let owner = Owner::default();
    let section = LampSection::new(&owner);
    // Dropped before it is polled at all.
    drop(section.patch(&set(30), false));
    // Cancelled while it waits for the section lock: the request is not
    // queued.
    {
        let (a, b) = (set(10), set(20));
        let mut first = pin!(section.patch(&a, false));
        assert!(poll_once(first.as_mut()).is_pending());
        owner.queue.take();
        let mut second = pin!(section.patch(&b, false));
        assert!(poll_once(second.as_mut()).is_pending());
        assert!(owner.queue.borrow().is_none());
    }
    assert!(owner.persisted.borrow().is_none());
    assert!(owner.published.borrow().is_empty());
    // The section is usable.
    drive(&owner, section.patch(&set(40), false)).unwrap();
    assert_eq!(*owner.published.borrow(), [(40, 0)]);
}

#[test]
fn a_failed_commit_publishes_nothing() {
    let owner = Owner::default();
    let section = LampSection::new(&owner);
    owner.fail_next.set(true);
    assert_eq!(
        drive(&owner, section.patch(&set(30), false)),
        Err(ConfigError::Unavailable)
    );
    assert!(owner.published.borrow().is_empty());
    assert!(owner.persisted.borrow().is_none());
}

#[test]
fn an_invalid_configuration_is_never_committed() {
    let owner = Owner::default();
    let section = LampSection::new(&owner);
    assert!(matches!(
        drive(&owner, section.patch(&set(300), false)),
        Err(ConfigError::Field(_))
    ));
    // One invalid mutation rejects the valid ones of the same patch.
    let mixed = [set(30)[0].clone(), field(2, 300)[0].clone()];
    assert!(matches!(
        drive(&owner, section.patch(&mixed, false)),
        Err(ConfigError::Field(_))
    ));
    assert!(owner.queue.borrow().is_none());
    assert!(owner.persisted.borrow().is_none());
    assert!(owner.published.borrow().is_empty());
}

#[test]
fn a_dry_run_writes_nothing() {
    let owner = Owner::default();
    let section = LampSection::new(&owner);
    drive(&owner, section.patch(&set(30), true)).unwrap();
    drive(&owner, section.reset(true)).unwrap();
    assert!(owner.queue.borrow().is_none());
    assert!(owner.persisted.borrow().is_none());
    assert!(owner.published.borrow().is_empty());
}

#[test]
fn a_cancelled_reset_is_still_committed() {
    let owner = Owner::default();
    let section = LampSection::new(&owner);
    drive(&owner, section.patch(&set(30), false)).unwrap();
    {
        let reset = section.reset(false);
        let mut reset = pin!(reset);
        assert!(poll_once(reset.as_mut()).is_pending());
    }
    owner.process();
    assert!(owner.persisted.borrow().is_none());
    assert_eq!(*owner.published.borrow(), [(30, 0), (0, 0)]);
}

#[test]
fn a_cancelled_service_update_is_still_committed() {
    let owner = Owner::default();
    let section = LampSection::new(&owner);
    {
        let update = section.update(|overrides| overrides.level = Override::Value(55));
        let mut update = pin!(update);
        assert!(poll_once(update.as_mut()).is_pending());
    }
    owner.process();
    assert_eq!(*owner.published.borrow(), [(55, 0)]);
}

#[test]
fn concurrent_changes_of_different_fields_are_both_kept() {
    let owner = Owner::default();
    let section = LampSection::new(&owner);
    // A client patch and a service update race for the section lock; the
    // second one is applied to what the first one committed.
    let (patch, update) = drive(
        &owner,
        join(
            section.patch(&set(30), false),
            section.update(|overrides| overrides.hue = Override::Value(7)),
        ),
    );
    patch.unwrap();
    update.unwrap();
    assert_eq!(*owner.published.borrow(), [(30, 0), (30, 7)]);
    let values = drive(&owner, section.get()).unwrap();
    assert_eq!(values.overridden.as_slice(), [1, 2]);
}

#[test]
fn the_last_committed_change_of_a_field_wins() {
    let owner = Owner::default();
    let section = LampSection::new(&owner);
    let (a, b) = drive(
        &owner,
        join(
            section.patch(&set(10), false),
            section.patch(&set(20), false),
        ),
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(*owner.published.borrow(), [(10, 0), (20, 0)]);
}
