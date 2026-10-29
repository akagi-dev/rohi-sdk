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

//! Firmware config bindings: how each persisted section reaches its runtime
//! consumer.
//!
//! ```text
//! C::default() + Binding::firmware_overrides() + user overrides (flash)
//!        = effective C  ──►  Binding::publish(&ConfigRuntime, &C)
//! ```
//!
//! A [`Binding`](crate::bindings::Binding) is the one firmware-local description of a managed section:
//! the product policy (the firmware overrides) and the consumer that gets the
//! validated effective value. It is a zero-sized marker type, not a registry or
//! a service locator: [`firmware_configs!`](crate::firmware_configs) lists the
//! bindings once, at compile time, and the configuration owner task uses that
//! list for boot, commits, removals and factory reset alike. A private persisted
//! section (the identity) is a [`PrivateBinding`](crate::bindings::PrivateBinding): it is not user-visible and
//! has no overrides.
//!
//! Portable feature crates know nothing of the bindings.

use core::cell::RefCell;
use core::fmt;
use core::marker::PhantomData;

use embassy_sync::blocking_mutex::Mutex;
use log::{info, warn};
use rohi_config::{ConfigSection, HasOverrides, materialize};
use rohi_connectivity::ConnectivityConfig;
use rohi_identity::Identity;
use rohi_location::LocationConfig;
use rohi_net::{BleSettings, NetworkConfig, WifiConfig, WifiHandle};
use rohi_service::{Config, SectionStore, StoreError};
use rohi_time_ntp::NtpServersConfig;

use crate::config::{ConfigHandle, ServiceError};
use crate::tasks::RawMutex;
use crate::{connectivity, identity, location, time};

/// The cheap handles runtime publication needs. Hardware and storage stay
/// single-owner elsewhere.
#[derive(Clone, Copy)]
pub struct ConfigRuntime {
    wifi: WifiHandle<'static, RawMutex>,
}

impl ConfigRuntime {
    /// The runtime behind `wifi`, the desired state of the Wi-Fi manager.
    pub const fn new(wifi: WifiHandle<'static, RawMutex>) -> Self {
        Self { wifi }
    }
}

/// The firmware side of a managed config section.
pub trait Binding {
    /// The effective section.
    type Config: Config;

    /// Whether a change only takes effect after a restart. Such a section is
    /// published once, at boot; a later commit only persists it.
    const RESTART_REQUIRED: bool = false;

    /// The product policy compiled into the firmware, never persisted; all
    /// `Default` when the product has none.
    fn firmware_overrides() -> <Self::Config as HasOverrides>::Overrides {
        Default::default()
    }

    /// The effective value without user overrides.
    fn firmware_only() -> Self::Config {
        materialize(&Self::firmware_overrides(), &Default::default())
    }

    /// Hands the validated effective value to its consumer. Synchronous and
    /// cheap: the owner task calls it in the commit step, with no `.await`
    /// between the commit and the publication.
    fn publish(runtime: &ConfigRuntime, effective: &Self::Config);
}

/// A persisted section the firmware keeps for itself: not a [`Config`],
/// no overrides, not visible to clients.
pub trait PrivateBinding {
    /// The stored section.
    type Section: ConfigSection;

    /// A committed (`Some`) or removed or never stored (`None`) section.
    fn publish(section: Option<&Self::Section>);

    /// The stored section could not be read at boot. Not the same as missing:
    /// the consumer must not assume the defaults.
    fn unreadable(error: &dyn fmt::Display);
}

/// The managed section of binding `B` behind the configuration owner task:
/// the firmware overrides of the binding and the persisted user overrides.
///
/// The owner task publishes the committed effective value to the runtime itself
/// with the same binding, see [`crate::config`]; cancelling a call can
/// therefore never leave a stored configuration unknown to the runtime.
pub struct Stored<B> {
    config: ConfigHandle,
    _binding: PhantomData<fn() -> B>,
}

impl<B> Clone for Stored<B> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<B> Copy for Stored<B> {}

impl<B> Stored<B> {
    /// The persisted section of `B` behind `config`.
    pub const fn new(config: ConfigHandle) -> Self {
        Self {
            config,
            _binding: PhantomData,
        }
    }
}

fn store_error(error: ServiceError) -> StoreError {
    match error {
        ServiceError::Unavailable(_) => StoreError::Unavailable,
        _ => StoreError::Internal,
    }
}

impl<B: Binding> SectionStore<B::Config> for Stored<B> {
    fn firmware_overrides(&self) -> <B::Config as HasOverrides>::Overrides {
        B::firmware_overrides()
    }

    async fn load(&self) -> Result<Option<<B::Config as HasOverrides>::Overrides>, StoreError> {
        self.config.get().await.map_err(store_error)
    }

    // The owner task publishes the effective value itself, from the same
    // binding, in the commit step.
    async fn store(
        &self,
        overrides: &<B::Config as HasOverrides>::Overrides,
        _effective: &B::Config,
    ) -> Result<(), StoreError> {
        self.config.set(overrides).await.map_err(store_error)
    }

    async fn remove(&self, _effective: &B::Config) -> Result<(), StoreError> {
        self.config
            .remove::<<B::Config as HasOverrides>::Overrides>()
            .await
            .map_err(store_error)
    }

    fn restart_required(&self) -> bool {
        B::RESTART_REQUIRED
    }
}

/// `wifi`: the desired state of the Wi-Fi manager.
pub struct WifiBinding;

impl Binding for WifiBinding {
    type Config = WifiConfig;

    fn publish(runtime: &ConfigRuntime, effective: &WifiConfig) {
        runtime.wifi.set_wifi_config(effective.clone());
    }
}

/// `network`: the desired IPv4 setup of the Wi-Fi manager.
pub struct NetworkBinding;

impl Binding for NetworkBinding {
    type Config = NetworkConfig;

    fn publish(runtime: &ConfigRuntime, effective: &NetworkConfig) {
        runtime.wifi.set_network_config(effective.clone());
    }
}

/// The BLE settings of the running peripheral, taken at boot.
static BLE_SETTINGS: Mutex<RawMutex, RefCell<BleSettings>> =
    Mutex::new(RefCell::new(BleSettings { name: None }));

/// The BLE settings of this boot: the name is read once, when the peripheral
/// starts.
pub fn ble_settings() -> BleSettings {
    BLE_SETTINGS.lock(|settings| settings.borrow().clone())
}

/// `ble`: the startup snapshot of the peripheral. Restart-required.
pub struct BleBinding;

impl Binding for BleBinding {
    type Config = BleSettings;
    const RESTART_REQUIRED: bool = true;

    fn publish(_: &ConfigRuntime, effective: &BleSettings) {
        BLE_SETTINGS.lock(|settings| *settings.borrow_mut() = effective.clone());
    }
}

/// `connectivity`: the telemetry task restarts with the new values. The
/// product policy is the build-time one, see [`connectivity::firmware_overrides`].
pub struct ConnectivityBinding;

impl Binding for ConnectivityBinding {
    type Config = ConnectivityConfig;

    fn firmware_overrides() -> rohi_connectivity::ConnectivityOverrides {
        connectivity::firmware_overrides()
    }

    fn publish(_: &ConfigRuntime, effective: &ConnectivityConfig) {
        connectivity::publish(effective.clone());
    }
}

/// `location`: the shared location of the telemetry frames and the `location`
/// service.
pub struct LocationBinding;

impl Binding for LocationBinding {
    type Config = LocationConfig;

    fn publish(_: &ConfigRuntime, effective: &LocationConfig) {
        location::shared().publish_configured(Some(effective));
    }
}

/// `time`: the NTP task restarts with the new servers.
pub struct TimeBinding;

impl Binding for TimeBinding {
    type Config = NtpServersConfig;

    fn publish(_: &ConfigRuntime, effective: &NtpServersConfig) {
        time::publish(effective.clone());
    }
}

/// The device identity: the signer of the telemetry frames and the control
/// plane.
pub struct IdentityBinding;

impl PrivateBinding for IdentityBinding {
    type Section = Identity;

    fn publish(section: Option<&Identity>) {
        match section {
            Some(identity) => {
                info!(
                    "Identity: provisioned, public key {}",
                    identity.public_key()
                );
                identity::set_signer(Some(identity.signer()));
            }
            None => {
                info!("Identity: not provisioned");
                identity::set_signer(None);
            }
        }
    }

    fn unreadable(error: &dyn fmt::Display) {
        warn!(
            "Identity: stored identity unreadable, state unknown: {}",
            error
        );
    }
}

/// Lists the firmware config bindings once and generates what the
/// configuration owner task and the control plane need from the list:
///
/// - `ConfigSections` and `config_sections(handle)`: the managed sections of
///   the control plane;
/// - `publish_section`: publishes a committed or removed section by id;
/// - `publish_boot`: reads and publishes every section at boot;
/// - `publish_factory_reset`: publishes the state after a factory reset.
///
/// Section ids are checked at compile time. It never instantiates a service
/// or spawns a task.
#[macro_export]
macro_rules! firmware_configs {
    (
        managed $($managed:ty),+ $(,)?;
        private $($private:ty),+ $(,)?
    ) => {
        /// The managed config sections of the device.
        pub type ConfigSections = (
            $(
                ::rohi_service::Section<
                    <$managed as $crate::bindings::Binding>::Config,
                    $crate::bindings::Stored<$managed>,
                    $crate::tasks::RawMutex,
                >,
            )+
        );

        /// The managed config sections of the device over `config`.
        pub fn config_sections(config: ConfigHandle) -> ConfigSections {
            ($(::rohi_service::Section::new($crate::bindings::Stored::<$managed>::new(config)),)+)
        }

        const _: () = assert!(
            ids_are_unique(&[
                $(managed_id::<$managed>(),)+
                $(private_id::<$private>(),)+
            ]),
            "two config bindings share a section id"
        );

        /// Publishes the committed (`Some`) or removed (`None`) section `id`.
        /// `boot`: the first publication of a start, which also covers
        /// restart-required sections and falls back to the firmware policy
        /// when the stored value is unusable.
        fn publish_section(
            runtime: &ConfigRuntime,
            id: SectionId,
            blob: Option<&Blob>,
            boot: bool,
        ) {
            $(
                if id == section_id::<$managed>() {
                    publish_managed::<$managed>(runtime, blob, boot);
                    return;
                }
            )+
            $(
                if id.0 == private_id::<$private>() {
                    publish_private::<$private>(blob);
                    return;
                }
            )+
        }

        /// Reads every section of `store` and publishes it, so that every
        /// consumer starts from the same effective value a commit would give.
        /// `None`: the store is unavailable, so the firmware policy applies and
        /// private sections stay unknown.
        fn publish_boot(store: Option<&mut ConfigStore<Storage>>, runtime: &ConfigRuntime) {
            let Some(store) = store else {
                $(publish_managed::<$managed>(runtime, None, true);)+
                $(<$private as $crate::bindings::PrivateBinding>::unreadable(
                    &"configuration unavailable",
                );)+
                return;
            };
            $(boot_managed::<$managed>(store, runtime);)+
            $(boot_private::<$private>(store);)+
        }

        /// Publishes the state after a factory reset: nothing is stored.
        fn publish_factory_reset(runtime: &ConfigRuntime) {
            $(publish_managed::<$managed>(runtime, None, false);)+
            $(publish_private::<$private>(None);)+
        }
    };
}
