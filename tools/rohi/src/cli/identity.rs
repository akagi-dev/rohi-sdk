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
//! `rohi identity`: show, provision and erase the device identity.

use sp_core::crypto::{AccountId32, Ss58AddressFormat, Ss58Codec};
use zeroize::Zeroizing;

use super::args::{IdentityAction, Provision};
use super::diagnostic::CliError;
use super::dto;
use super::{Ctx, Prepared};
use crate::model::{IdentityInfo, PublicKey};
use crate::{ClientTransport, DerivedIdentity, Device, Error, derive_identity};

/// SS58 network prefix of Robonomics.
const ROBONOMICS_SS58_PREFIX: u16 = 32;

/// The public key as a Robonomics (SS58) address.
pub fn address(key: &PublicKey) -> String {
    AccountId32::new(key.to_bytes())
        .to_ss58check_with_version(Ss58AddressFormat::custom(ROBONOMICS_SS58_PREFIX))
}

/// Human text of the identity: the lines of the `Identity` section.
pub fn lines(identity: &IdentityInfo) -> Vec<(&'static str, String)> {
    let mut lines = vec![("State", identity.state.to_string())];
    if let Some(key) = &identity.public_key {
        lines.push(("Address", address(key)));
        lines.push(("Public key", key.to_string()));
    }
    lines
}

fn text(identity: &IdentityInfo) -> String {
    let mut out = String::new();
    for (name, value) in lines(identity) {
        out.push_str(&format!("{name:<12}{value}\n"));
    }
    out
}

/// The identity derived from the seed file of `provision`, before any device
/// is touched.
pub fn load(provision: &Provision) -> Result<DerivedIdentity, CliError> {
    let seed = std::fs::read_to_string(&provision.seed).map_err(|error| {
        Error::InvalidInput(format!(
            "cannot read the seed file {}: {error}",
            provision.seed.display()
        ))
    })?;
    let seed = Zeroizing::new(seed);
    Ok(derive_identity(seed.trim(), provision.number)?)
}

pub async fn run<T: ClientTransport>(
    device: &mut Device<T>,
    action: Option<IdentityAction>,
    prepared: Prepared,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    match action {
        None => {
            let identity = device.identity().await?;
            ctx.io.emit(
                ctx.format,
                &dto::Versioned::new(dto::Identity::new(&identity)),
                || text(&identity),
            )?;
        }
        Some(IdentityAction::Provision(_)) => {
            let derived = prepared.identity.ok_or_else(|| {
                Error::InvalidInput("the identity was not derived before connecting".into())
            })?;
            let public_key = device.provision_identity(&derived).await?;
            let out = dto::Provisioned {
                ok: true,
                address: address(&public_key),
                public_key: public_key.to_string(),
            };
            ctx.io.emit(ctx.format, &dto::Versioned::new(&out), || {
                format!(
                    "Identity provisioned\n\nAddress:    {}\nPublic key: {public_key}\n",
                    out.address
                )
            })?;
        }
        Some(IdentityAction::Erase { yes }) => {
            ctx.io
                .confirm(
                    "This device will lose its current cryptographic identity.\n\
                     It must be provisioned again before signed telemetry can be produced.\n\n\
                     Continue?",
                    yes,
                )
                .await?;
            device.erase_identity().await?;
            ctx.io
                .emit(ctx.format, &dto::done(), || "Identity erased\n".to_owned())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_is_a_robonomics_ss58_address_of_the_key() {
        let alice_ed25519: [u8; 32] = [
            0x88, 0xdc, 0x34, 0x17, 0xd5, 0x05, 0x8e, 0xc4, 0xb4, 0x50, 0x3e, 0x0c, 0x12, 0xea,
            0x1a, 0x0a, 0x89, 0xbe, 0x20, 0x0f, 0xe9, 0x89, 0x22, 0x42, 0x3d, 0x43, 0x34, 0x01,
            0x4f, 0xa6, 0xb0, 0xee,
        ];
        let key = PublicKey::from_bytes(alice_ed25519);
        let address = address(&key);

        let (decoded, format) = AccountId32::from_ss58check_with_version(&address).unwrap();
        assert_eq!(<[u8; 32]>::from(decoded), alice_ed25519);
        assert_eq!(u16::from(format), ROBONOMICS_SS58_PREFIX);
        // The same key under the generic Substrate prefix is the well-known
        // address of Alice's Ed25519 key.
        assert_eq!(
            AccountId32::new(alice_ed25519)
                .to_ss58check_with_version(Ss58AddressFormat::custom(42)),
            "5FA9nQDVg267DEd8m1ZypXLBnvN7SFxYwV7ndqSYGiN9TTpu"
        );
        assert!(address.starts_with('4'), "{address}");
    }
}
