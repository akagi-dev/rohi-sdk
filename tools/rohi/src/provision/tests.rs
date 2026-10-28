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

use sp_core::crypto::Pair as _;
use sp_core::ed25519::Pair;

use super::*;

/// The public Substrate development phrase. Never a production master.
const DEV_PHRASE: &str = "bottom drive obey lake curtain smoke basket hold race lonely fit walk";

fn hex(key: PublicKey) -> String {
    key.to_string()
}

// The provisioning ABI: changing any of these means the same (master,
// number) no longer reproduces the same device identity.
const GOLDEN: [(u64, &str); 3] = [
    (
        1,
        "0xbf3a763d817cee09bf785b9cc6118f58dab5c03f3ace6d524899bcb28ac74f27",
    ),
    (
        2,
        "0x276b9ce3c58b9f009f4f1f2ae5c7e9a599796246165df52f2d8b857fcecb1a40",
    ),
    (
        42,
        "0x8743ca72162320542b15ef0e66995fac97cd295626627046c41f64f796bcf2f3",
    ),
];

#[test]
fn golden_derivation_vectors() {
    for (number, public) in GOLDEN {
        let derived = derive_identity(DEV_PHRASE, number).unwrap();
        assert_eq!(hex(derived.public_key()), public, "//{number}");
    }
}

#[test]
fn derivation_is_exactly_hard_junction_of_the_number() {
    for number in [0, 1, 2, 42, 1000, u64::MAX] {
        let derived = derive_identity(DEV_PHRASE, number).unwrap();
        let reference = Pair::from_string(&format!("{DEV_PHRASE}//{number}"), None).unwrap();
        assert_eq!(derived.public_key().to_bytes(), reference.public().0);
        assert_eq!(derived.secret().expose_secret(), &reference.seed());
    }
}

#[test]
fn no_namespace_is_added() {
    let derived = derive_identity(DEV_PHRASE, 42).unwrap();
    for wrong in ["//rohi//42", "//sensor//42", "//rohi/42", "/42"] {
        let other = Pair::from_string(&format!("{DEV_PHRASE}{wrong}"), None);
        if let Ok(other) = other {
            assert_ne!(derived.public_key().to_bytes(), other.public().0, "{wrong}");
        }
    }
}

#[test]
fn same_master_and_number_reproduce_the_identity() {
    let a = derive_identity(DEV_PHRASE, 42).unwrap();
    let b = derive_identity(DEV_PHRASE, 42).unwrap();
    assert_eq!(a.public_key(), b.public_key());
    assert_eq!(a.secret().expose_secret(), b.secret().expose_secret());
}

#[test]
fn different_numbers_or_masters_give_different_identities() {
    let one = derive_identity(DEV_PHRASE, 1).unwrap();
    let two = derive_identity(DEV_PHRASE, 2).unwrap();
    assert_ne!(one.public_key(), two.public_key());
    let other = derive_identity("//Alice", 1).unwrap();
    assert_ne!(one.public_key(), other.public_key());
}

#[test]
fn hex_seed_masters_are_supported() {
    let seed = format!("0x{}", "11".repeat(32));
    let derived = derive_identity(&seed, 7).unwrap();
    let reference = Pair::from_string(&format!("{seed}//7"), None).unwrap();
    assert_eq!(derived.public_key().to_bytes(), reference.public().0);
}

#[test]
fn password_of_the_master_is_applied_before_the_number() {
    let suri = format!("{DEV_PHRASE}///hunter2");
    let derived = derive_identity(&suri, 3).unwrap();
    let (master, _) = Pair::from_string_with_seed(&suri, None).unwrap();
    let (child, _) = master
        .derive(
            core::iter::once(sp_core::crypto::DeriveJunction::hard(3u64)),
            None,
        )
        .unwrap();
    assert_eq!(derived.public_key().to_bytes(), child.public().0);
    assert_ne!(
        derived.public_key(),
        derive_identity(DEV_PHRASE, 3).unwrap().public_key()
    );
}

#[test]
fn device_side_key_derivation_matches_sp_core() {
    for number in [1, 2, 42] {
        let derived = derive_identity(DEV_PHRASE, number).unwrap();
        let device = Identity::from_secret(derived.secret().expose_secret()).unwrap();
        assert_eq!(device.public_key(), derived.public_key());
        assert_eq!(
            DerivedIdentity::from_secret(*derived.secret().expose_secret()).public_key(),
            derived.public_key()
        );
    }
}

#[test]
fn invalid_master_is_rejected_without_echoing_it() {
    let error = derive_identity("not a valid phrase at all", 1)
        .err()
        .unwrap();
    let text = error.to_string();
    assert!(!text.contains("not a valid phrase"), "{text}");
}

#[test]
fn debug_shows_only_the_public_key() {
    let derived = derive_identity(DEV_PHRASE, 1).unwrap();
    let debug = format!("{derived:?}");
    assert!(debug.contains(&hex(derived.public_key())));
    assert!(!debug.contains("secret"), "{debug}");
}
