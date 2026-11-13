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

use core::cell::Cell;

use embassy_futures::block_on;
use rohi_service::{ActionRegistry, Service, Value};

use super::*;

#[derive(Default)]
struct Fake {
    reboots: Cell<u32>,
    resets: Cell<u32>,
    reboot: Cell<Option<SystemError>>,
    reset: Cell<Option<SystemError>>,
    restart_required: Cell<bool>,
}

impl SystemPlatform for Fake {
    fn schedule_reboot(&self) -> Result<(), SystemError> {
        match self.reboot.get() {
            Some(error) => Err(error),
            None => {
                self.reboots.set(self.reboots.get() + 1);
                Ok(())
            }
        }
    }

    async fn factory_reset(&self) -> Result<FactoryReset, SystemError> {
        match self.reset.get() {
            Some(error) => Err(error),
            None => {
                self.resets.set(self.resets.get() + 1);
                Ok(FactoryReset {
                    restart_required: self.restart_required.get(),
                })
            }
        }
    }
}

const ID: u32 = SystemService::<Fake>::ID;

#[test]
fn the_service_is_called_system_and_has_only_actions() {
    assert_eq!(SystemService::<Fake>::NAME, "system");
    let system = SystemService(Fake::default());
    let actions = (&system,);
    assert_eq!(actions.service_count(), 1);
    assert_eq!(actions.service_identity(0).unwrap().name, "system");
    const { assert!(<(&SystemService<Fake>,) as ActionRegistry>::SCHEMAS_VALID) };
}

#[test]
fn both_actions_are_declared_with_their_access_and_no_secrets() {
    let actions = <SystemService<Fake> as Action>::ACTIONS;
    assert_eq!(actions.len(), 2);

    let reboot = &actions[0];
    assert_eq!((reboot.id, reboot.name), (1, "reboot"));
    assert_eq!(reboot.access, ActionAccess::Mutate);
    assert!(reboot.args.is_empty() && reboot.results.is_empty());

    let reset = &actions[1];
    assert_eq!((reset.id, reset.name), (2, "factory_reset"));
    assert_eq!(reset.access, ActionAccess::Provision);
    assert!(reset.args.is_empty());
    assert_eq!(reset.results.len(), 1);
    assert_eq!(
        (
            reset.results[0].id,
            reset.results[0].name,
            reset.results[0].kind
        ),
        (1, "restart_required", ValueKind::Bool)
    );

    assert!(
        actions
            .iter()
            .all(|action| !action.secret_args && !action.stream)
    );
}

#[test]
fn the_registry_resolves_both_actions() {
    let system = SystemService(Fake::default());
    let actions = (&system,);
    assert_eq!(actions.action(ID, 1).unwrap().name, "reboot");
    assert_eq!(actions.action(ID, 2).unwrap().name, "factory_reset");
    assert_eq!(actions.action(ID, 3), Err(ActionError::UnknownAction));
}

#[test]
fn reboot_is_scheduled_and_answers_without_results() {
    let system = SystemService(Fake::default());
    let actions = (&system,);
    let done = block_on(actions.invoke(ID, action::REBOOT, &[])).unwrap();
    assert!(done.records().is_empty());
    assert_eq!(system.0.reboots.get(), 1);
}

#[test]
fn a_reboot_that_cannot_be_scheduled_is_an_error() {
    let system = SystemService(Fake::default());
    system.0.reboot.set(Some(SystemError::Unavailable));
    let actions = (&system,);
    assert_eq!(
        block_on(actions.invoke(ID, action::REBOOT, &[])),
        Err(ActionError::Unavailable)
    );
    assert_eq!(system.0.reboots.get(), 0);
}

#[test]
fn factory_reset_reports_whether_a_restart_is_required_and_does_not_reboot() {
    for required in [false, true] {
        let system = SystemService(Fake::default());
        system.0.restart_required.set(required);
        let actions = (&system,);
        let done = block_on(actions.invoke(ID, action::FACTORY_RESET, &[])).unwrap();
        assert_eq!(
            done.records(),
            [[(result_field::RESTART_REQUIRED, Value::Bool(required))]]
        );
        assert_eq!(system.0.resets.get(), 1);
        assert_eq!(system.0.reboots.get(), 0);
    }
}

#[test]
fn factory_reset_failures_are_action_errors() {
    for (platform, action_error) in [
        (SystemError::Unavailable, ActionError::Unavailable),
        (SystemError::Internal, ActionError::Internal),
    ] {
        let system = SystemService(Fake::default());
        system.0.reset.set(Some(platform));
        let actions = (&system,);
        assert_eq!(
            block_on(actions.invoke(ID, action::FACTORY_RESET, &[])),
            Err(action_error)
        );
        assert_eq!(system.0.reboots.get(), 0);
    }
}

#[test]
fn unexpected_arguments_are_rejected_before_the_platform_runs() {
    let system = SystemService(Fake::default());
    let actions = (&system,);
    let extra = [(1, Value::Bool(true))];
    for id in [action::REBOOT, action::FACTORY_RESET] {
        assert_eq!(
            block_on(actions.invoke(ID, id, &extra)),
            Err(ActionError::InvalidArgument)
        );
    }
    assert_eq!(system.0.reboots.get() + system.0.resets.get(), 0);
}

#[test]
fn the_service_id_is_part_of_the_wire_abi() {
    // The FNV-1a 32-bit hash of "system".
    let mut hash: u32 = 0x811c_9dc5;
    for byte in b"system" {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    assert_eq!(ID, hash);
}
