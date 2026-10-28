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

use std::format;
use std::vec::Vec;

use minicbor::{CborLen, Decode, Encode};
use rohi_config::{ConfigSection, ConfigStore, MemoryStorage, SectionId};

use crate::{
    Identity, IdentityError, IdentitySigner, MockSigner, PublicKey, SECRET_LEN, Secret, Signature,
    Signer,
};

fn unhex<const N: usize>(text: &str) -> [u8; N] {
    let bytes: Vec<u8> = (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect();
    bytes.try_into().unwrap()
}

/// RFC 8032, section 7.1, TEST 1 and TEST 2.
const RFC8032: [(&str, &str, &str, &str); 2] = [
    (
        "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
        "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
        "",
        "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
    ),
    (
        "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
        "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
        "72",
        "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
    ),
];

fn identity(seed: u8) -> Identity {
    Identity::from_secret(&[seed; SECRET_LEN]).unwrap()
}

#[test]
fn rfc8032_vectors() {
    for (secret, public, message, signature) in RFC8032 {
        let identity = Identity::from_secret(&unhex::<32>(secret)).unwrap();
        assert_eq!(identity.public_key().to_bytes(), unhex::<32>(public));
        let message: Vec<u8> = (0..message.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&message[i..i + 2], 16).unwrap())
            .collect();
        let signed = identity.signer().sign(&message).unwrap();
        assert_eq!(signed.to_bytes(), unhex::<64>(signature));
        assert_eq!(identity.public_key().verify(&message, &signed), Ok(()));
    }
}

#[test]
fn sign_then_verify() {
    let identity = identity(1);
    let signer = identity.signer();
    assert_eq!(signer.public_key(), identity.public_key());
    let signature = signer.sign(b"exact payload bytes").unwrap();
    assert_eq!(
        identity
            .public_key()
            .verify(b"exact payload bytes", &signature),
        Ok(())
    );
}

#[test]
fn different_message_does_not_verify() {
    let identity = identity(1);
    let signature = identity.signer().sign(b"payload").unwrap();
    assert_eq!(
        identity.public_key().verify(b"payloaD", &signature),
        Err(IdentityError::InvalidSignature)
    );
}

#[test]
fn corrupted_signature_does_not_verify() {
    let identity = identity(1);
    let mut bytes = identity.signer().sign(b"payload").unwrap().to_bytes();
    bytes[10] ^= 0x01;
    assert_eq!(
        identity
            .public_key()
            .verify(b"payload", &Signature::from_bytes(bytes)),
        Err(IdentityError::InvalidSignature)
    );
}

#[test]
fn other_key_does_not_verify() {
    let signature = identity(1).signer().sign(b"payload").unwrap();
    assert_eq!(
        identity(2).public_key().verify(b"payload", &signature),
        Err(IdentityError::InvalidSignature)
    );
}

#[test]
fn different_secrets_give_different_keys() {
    assert_ne!(identity(1).public_key(), identity(2).public_key());
    assert_eq!(identity(1).public_key(), identity(1).public_key());
}

#[test]
fn secret_must_be_32_bytes() {
    for len in [0, 1, 31, 33, 64] {
        assert_eq!(
            Identity::from_secret(&std::vec![7; len]).err(),
            Some(IdentityError::InvalidSecret)
        );
    }
    assert!(Identity::from_secret(&[0; 32]).is_ok());
}

#[test]
fn debug_never_exposes_the_secret() {
    let identity = Identity::from_secret(&[0xab; 32]).unwrap();
    let secret_hex = "abababab";
    for text in [
        format!("{identity:?}"),
        format!("{identity:#?}"),
        format!("{:?}", identity.signer()),
        format!("{:?}", Secret::new([0xabu8; 32])),
        format!("{:?}", MockSigner::new([0xab; 32])),
    ] {
        assert!(!text.contains(secret_hex), "{text}");
        assert!(!text.contains("171,"), "{text}");
    }
    assert_eq!(
        format!("{:?}", Secret::new([0xabu8; 32])),
        "Secret(<redacted>)"
    );
    let public = format!("{}", identity.public_key());
    assert!(format!("{identity:?}").contains(&public));
}

#[test]
fn public_key_display() {
    let key = PublicKey::from_bytes([0x0f; 32]);
    let text = format!("{key}");
    assert_eq!(text.len(), 2 + 64);
    assert!(text.starts_with("0x0f0f"));
    assert_eq!(PublicKey::from_slice(&[0x0f; 32]), Some(key));
    assert_eq!(PublicKey::from_slice(&[0x0f; 31]), None);
}

#[test]
fn signer_requires_provisioned_identity() {
    assert_eq!(
        IdentitySigner::provisioned(None).err(),
        Some(IdentityError::NotProvisioned)
    );
    let identity = identity(3);
    assert_eq!(
        IdentitySigner::provisioned(Some(&identity))
            .unwrap()
            .public_key(),
        identity.public_key()
    );
}

#[test]
fn mock_signer() {
    let mock = MockSigner::default();
    let signature = mock.sign(b"m").unwrap();
    assert_eq!(mock.public_key().verify(b"m", &signature), Ok(()));
    let failing = MockSigner::failing(IdentityError::NotProvisioned);
    assert_eq!(failing.sign(b"m"), Err(IdentityError::NotProvisioned));
}

// Changing these values is a breaking persistent storage change.
#[test]
fn section_id_and_version_are_stable() {
    assert_eq!(Identity::ID, SectionId(0x94d6_3573_d312_fe9b));
    assert_eq!(Identity::ID, SectionId::from_name("Identity"));
    assert_eq!(Identity::VERSION, 1);
}

#[test]
fn cbor_encoding_is_stable() {
    let identity = Identity::from_secret(&[0x11; 32]).unwrap();
    let mut buffer = [0u8; 64];
    let len = rohi_config::encode_to_slice(&identity, &mut buffer).unwrap();
    let mut expected = std::vec![0x81, 0x58, 0x20];
    expected.extend_from_slice(&[0x11; 32]);
    assert_eq!(&buffer[..len], expected.as_slice());
    assert_eq!(identity.cbor_len(&mut ()), expected.len());

    let decoded: Identity = Identity::decode_versioned(1, &expected).unwrap();
    assert_eq!(decoded.public_key(), identity.public_key());
}

#[test]
fn cbor_rejects_wrong_secret_length() {
    let mut payload = std::vec![0x81, 0x58, 0x1f];
    payload.extend_from_slice(&[0x11; 31]);
    assert!(Identity::decode_versioned(1, &payload).is_err());
    assert!(Identity::decode_versioned(2, &[0x81, 0x40]).is_err());
}

/// An unrelated feature section.
#[derive(Debug, PartialEq, Encode, Decode, CborLen, rohi_config::ConfigSection)]
#[cbor(array)]
#[config(version = 1)]
struct TelemetryConfig {
    #[n(0)]
    interval_s: u32,
}

type Store = ConfigStore<MemoryStorage<16384>>;

fn reopen(store: Store) -> Store {
    ConfigStore::open(store.into_storage()).unwrap()
}

fn stored_key(store: &mut Store) -> Option<PublicKey> {
    store
        .get::<Identity>()
        .unwrap()
        .map(|identity| identity.public_key())
}

#[test]
fn identity_persists_across_reopen() {
    let identity = identity(9);
    let mut store = ConfigStore::open(MemoryStorage::<16384>::new()).unwrap();
    assert_eq!(stored_key(&mut store), None);
    store.set(&identity).unwrap();
    let mut store = reopen(store);
    assert_eq!(stored_key(&mut store), Some(identity.public_key()));
}

#[test]
fn unrelated_configuration_changes_preserve_identity() {
    let identity = identity(9);
    let mut store = ConfigStore::open(MemoryStorage::<16384>::new()).unwrap();
    store.set(&identity).unwrap();

    store.set(&TelemetryConfig { interval_s: 60 }).unwrap();
    store.set(&TelemetryConfig { interval_s: 30 }).unwrap();
    let mut store = reopen(store);
    assert_eq!(stored_key(&mut store), Some(identity.public_key()));

    store.remove::<TelemetryConfig>().unwrap();
    let mut store = reopen(store);
    assert_eq!(stored_key(&mut store), Some(identity.public_key()));
    assert_eq!(store.get::<TelemetryConfig>().unwrap(), None);
}

#[test]
fn erase_removes_only_identity() {
    let mut store = ConfigStore::open(MemoryStorage::<16384>::new()).unwrap();
    store.set(&identity(9)).unwrap();
    store.set(&TelemetryConfig { interval_s: 60 }).unwrap();
    store.remove::<Identity>().unwrap();
    let mut store = reopen(store);
    assert_eq!(stored_key(&mut store), None);
    assert_eq!(
        store.get::<TelemetryConfig>().unwrap(),
        Some(TelemetryConfig { interval_s: 60 })
    );
}
