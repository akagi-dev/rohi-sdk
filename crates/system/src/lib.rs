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
#![no_std]

//! The `system` service: reboot and factory reset as ordinary Actions, and why the device started.
//!
//! # Overview
//!
//! `rohi-system` provides [`SystemService`], a service named `system` that every
//! ROHI firmware can register in its
//! [`ActionRegistry`](rohi_service::ActionRegistry) and, to report why the
//! device started and what its last panic was, in its
//! [`Statuses`](rohi_service::Statuses). It has no config section. What the
//! device really does (when and how it resets, which storage it formats, what
//! it remembers of a crash) belongs to the platform and is injected through
//! [`SystemPlatform`]; the service itself knows no chip, flash or product.
//!
//! The crate is `no_std`, does not allocate and has no Cargo features. It
//! depends on [`rohi_service`] and on [`rohi_crash`], whose [`Report`]
//! is the data of the status.
//!
//! # Quick Example
//!
//! A platform that counts what it was asked for, and the two Actions run
//! through the registry the control plane uses:
//!
//! ```
//! use core::cell::Cell;
//!
//! use embassy_futures::block_on;
//! use rohi_service::{ActionRegistry, FactoryReset, Service, Value};
//! use rohi_system::{SystemError, SystemPlatform, SystemService, result_field};
//!
//! #[derive(Default)]
//! struct Platform {
//!     reboots: Cell<u32>,
//! }
//!
//! impl SystemPlatform for Platform {
//!     fn schedule_reboot(&self) -> Result<(), SystemError> {
//!         self.reboots.set(self.reboots.get() + 1);
//!         Ok(())
//!     }
//!
//!     async fn factory_reset(&self) -> Result<FactoryReset, SystemError> {
//!         Ok(FactoryReset { restart_required: true })
//!     }
//! }
//!
//! let system = SystemService(Platform::default());
//! let actions = (&system,);
//!
//! // Factory reset reports that a restart is needed and does not reboot.
//! let done = block_on(actions.invoke(SystemService::<Platform>::ID, 2, &[])).unwrap();
//! assert_eq!(
//!     done.records()[0][0],
//!     (result_field::RESTART_REQUIRED, Value::Bool(true))
//! );
//! assert_eq!(system.0.reboots.get(), 0);
//!
//! // Rebooting is a separate Action.
//! block_on(actions.invoke(SystemService::<Platform>::ID, 1, &[])).unwrap();
//! assert_eq!(system.0.reboots.get(), 1);
//! ```
//!
//! # Core Concepts
//!
//! | Concept | Meaning |
//! |---------|---------|
//! | [`SystemService`] | the `system` service over a [`SystemPlatform`] |
//! | [`SystemPlatform`] | what the platform provides: deferred reboot, the storage-aware factory reset and, optionally, the boot [`Report`] |
//! | [`SystemError`] | why the platform could not do it |
//! | status | why the device started (`reset_reason`: `power_on`, `software`, `panic`, `watchdog`, `brownout`, ...) and the last panic, see [`status_field`] and [`issue`] |
//!
//! | Action | Id | Arguments | Results | Access |
//! |--------|----|-----------|---------|--------|
//! | `reboot` | [`action::REBOOT`] | none | none | [`Mutate`](rohi_service::ActionAccess::Mutate) |
//! | `factory_reset` | [`action::FACTORY_RESET`] | none | `restart_required: Bool` ([`result_field::RESTART_REQUIRED`]) | [`Provision`](rohi_service::ActionAccess::Provision) |
//!
//! The access levels are declared, not enforced here: the link policy of
//! [`rohi-control`](../rohi_control/index.html) decides. Neither Action takes
//! secret arguments. Arguments are rejected by the registry before the
//! service runs.
//!
//! # Architecture and Integration
//!
//! ```text
//! rohi <service> <action>  ──►  InvokeAction  ──►  rohi-control (access check)
//!                                                       │
//!                                           ActionRegistry (.., &SystemService<P>)
//!                                                       │
//!                                  SystemPlatform  (firmware, per platform)
//!                                   ├─ schedule_reboot: signal, reset after a grace period
//!                                   ├─ factory_reset:   format the configuration storage
//!                                   └─ boot_report:     reset reason and last panic (rohi-crash)
//!
//! rohi status system  ──►  StatusRegistry (.., &SystemService<P>)  ──►  boot_report
//! ```
//!
//! A firmware implements [`SystemPlatform`] once per platform and passes
//! `&SystemService<P>` in the tuple of its action registry and, to report the
//! start and the last crash, in the tuple of its status registry. Altruist
//! Urban does this in `altruist-urban`, so another firmware reuses the service
//! and writes only the adapter.
//!
//! # API Navigation
//!
//! | Item | Use it to |
//! |------|-----------|
//! | [`SystemService`] | register the `system` Actions and status |
//! | [`SystemPlatform`] | implement reboot, factory reset and the boot report on a platform |
//! | [`SystemError`] | report that the platform cannot do the operation |
//! | [`action`], [`result_field`] | the stable ids of the Actions and of the result field |
//! | [`status_field`], [`issue`] | the stable ids of the status fields and of the issue codes |
//!
//! # Guarantees and Limitations
//!
//! - **Reboot is deferred.** `system.reboot` only calls
//!   [`SystemPlatform::schedule_reboot`], which must return before the device
//!   resets, so the response of the Action is delivered. The grace period is
//!   the platform's.
//! - **Factory reset does not reboot.** It reports
//!   [`FactoryReset::restart_required`](rohi_service::FactoryReset::restart_required)
//!   as `restart_required`; the client runs `system.reboot` when it wants a
//!   restart. What the reset removes is defined by the platform's
//!   [`SystemPlatform::factory_reset`]; persistent logs and unrelated storage
//!   are not part of the contract of this crate.
//! - **Status is optional and diagnostic.** Without a
//!   [`boot_report`](SystemPlatform::boot_report) the status has no fields and
//!   the service is `running / ok / ready`. With one, the service is
//!   `degraded` while the last start was one of a crash loop; the issues are
//!   `crashed`, `crash_loop`, `crash_not_logged` and `unexpected_reset`. The
//!   service is not [`ESSENTIAL`](rohi_service::Status::ESSENTIAL). A text field is at
//!   most 32 bytes: `last_crash` is the **start** of the panic message and
//!   `last_crash_at` the end of the file and the line; the whole record is in
//!   the persistent log.
//! - **Errors.** A platform failure is the Action error
//!   [`Unavailable`](rohi_service::ActionError::Unavailable) or
//!   [`Internal`](rohi_service::ActionError::Internal), see [`SystemError`].
//! - **Wire ABI.** The service id derives from the name `system`; Action ids,
//!   the result field id, the status field ids and the issue codes are part of
//!   the wire contract and are only added, never reused.
//! - **Cancellation.** The service adds no state: cancelling the Action
//!   future cancels what [`SystemPlatform::factory_reset`] does, so the
//!   platform documents its own cancellation behaviour.
//! - **Not verified here.** Chip reset and flash formatting are done by the
//!   platform adapters; this crate is tested on the host with fake platforms.

use rohi_crash::Report;
use rohi_service::{
    Action, ActionAccess, ActionDescriptor, ActionError, ActionOutput, FactoryReset,
    FieldDescriptor, Service, Value, ValueKind,
};

mod boot;

pub use boot::{issue, status_field};

/// Ids of the Actions of the `system` service.
pub mod action {
    /// `reboot`.
    pub const REBOOT: u32 = 1;
    /// `factory_reset`.
    pub const FACTORY_RESET: u32 = 2;
}

/// Ids of the result fields of the Actions of the `system` service.
pub mod result_field {
    /// `restart_required` of `factory_reset`: the change only takes effect
    /// after a restart.
    pub const RESTART_REQUIRED: u32 = 1;
}

/// Why a [`SystemPlatform`] could not do what was asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemError {
    /// The platform cannot do it now or at all: a platform that cannot reboot,
    /// or a missing configuration partition. Becomes
    /// [`ActionError::Unavailable`].
    Unavailable,
    /// Unexpected failure of the platform. Becomes [`ActionError::Internal`].
    Internal,
}

impl From<SystemError> for ActionError {
    fn from(error: SystemError) -> Self {
        match error {
            SystemError::Unavailable => ActionError::Unavailable,
            SystemError::Internal => ActionError::Internal,
        }
    }
}

/// What a platform provides to the [`SystemService`].
///
/// An implementation lives in the firmware of the platform (the ESP chip, the
/// host simulator): it owns the reset and the storage, this crate owns the
/// Actions.
#[allow(async_fn_in_trait)]
pub trait SystemPlatform {
    /// Schedules a reboot of the device.
    ///
    /// It must **return before the device resets**: the response of the
    /// Action still has to reach the client, so the platform defers the reset
    /// (for example by signalling a task that resets after a grace period).
    ///
    /// # Errors
    ///
    /// [`SystemError::Unavailable`] when the device cannot reboot or the
    /// reboot cannot be scheduled.
    fn schedule_reboot(&self) -> Result<(), SystemError>;

    /// Returns the configuration of the device to its factory state: the whole
    /// configuration storage is formatted, including unknown and obsolete
    /// sections, the stored identity and every override, and the runtime goes
    /// back to the configuration defaults. Persistent logs and unrelated
    /// storage are left alone. It must not reboot.
    ///
    /// A storage that is corrupted but can be formatted is repaired.
    ///
    /// # Errors
    ///
    /// [`SystemError::Unavailable`] when there is no storage to format,
    /// [`SystemError::Internal`] for any other failure.
    async fn factory_reset(&self) -> Result<FactoryReset, SystemError>;

    /// Why the device started and what its last panic was, for the `system`
    /// status; `None` (the default) when the platform keeps no such record.
    ///
    /// Called on every status request, so it must be cheap and must not block.
    fn boot_report(&self) -> Option<Report> {
        None
    }
}

/// The `system` service: Actions `reboot` and `factory_reset` over the
/// [`SystemPlatform`] `P`.
///
/// It has no Config capability. Register `&SystemService<P>` in the action
/// registry, and in the status registry to report the start and the last
/// crash (see [`SystemPlatform::boot_report`]).
#[derive(Service)]
pub struct SystemService<P>(pub P);

impl<P: SystemPlatform> Action for SystemService<P> {
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(action::REBOOT, "reboot", ActionAccess::Mutate),
        ActionDescriptor::new(
            action::FACTORY_RESET,
            "factory_reset",
            ActionAccess::Provision,
        )
        .results(&[FieldDescriptor::new(
            result_field::RESTART_REQUIRED,
            "restart_required",
            ValueKind::Bool,
        )]),
    ];

    async fn invoke(
        &self,
        action: u32,
        args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        if !args.is_empty() {
            return Err(ActionError::InvalidArgument);
        }
        match action {
            action::REBOOT => Ok(self.0.schedule_reboot()?),
            action::FACTORY_RESET => {
                let done = self.0.factory_reset().await?;
                out.bool(result_field::RESTART_REQUIRED, done.restart_required)
                    .map_err(|_| ActionError::Internal)
            }
            _ => Err(ActionError::UnknownAction),
        }
    }
}

#[cfg(test)]
mod tests;
