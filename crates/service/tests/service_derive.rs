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

use rohi_service::{Service, distinct_ids, service_id, service_name_is_valid};

#[derive(Service)]
struct WifiService;

#[derive(Service)]
#[service(name = "wifi")]
struct WirelessService;

#[derive(Service)]
struct TimeSyncService;

#[derive(Service)]
struct Generic<'a, T>(core::marker::PhantomData<&'a T>);

// Changing any of these values is a breaking change of the wire ABI.
#[test]
fn service_id_golden_values() {
    assert_eq!(service_id(""), 0x811c_9dc5);
    assert_eq!(service_id("a"), 0xe40c_292c);
    assert_eq!(service_id("foobar"), 0xbf9c_f968);
    assert_eq!(service_id("wifi"), 0x0728_6448);
}

#[test]
fn derived_identity_is_deterministic() {
    assert_eq!(WifiService::NAME, "wifi");
    assert_eq!(WifiService::ID, service_id("wifi"));
    assert_eq!(TimeSyncService::NAME, "time_sync");
    assert_eq!(TimeSyncService::ID, service_id("time_sync"));
}

#[test]
fn explicit_name_wins_and_ids_follow_the_name_only() {
    assert_eq!(WirelessService::NAME, "wifi");
    assert_eq!(WirelessService::ID, WifiService::ID);
}

#[test]
fn identity_ignores_type_parameters() {
    assert_eq!(<Generic<'static, u8>>::NAME, "generic");
    assert_eq!(<Generic<'static, u8>>::ID, <Generic<'static, u32>>::ID);
}

#[test]
fn ids_are_checked_in_const_context() {
    const IDS: [u32; 3] = [WifiService::ID, TimeSyncService::ID, Generic::<u8>::ID];
    const _: () = assert!(distinct_ids(&IDS));
    assert!(!distinct_ids(&[1, 2, 1]));
    assert!(distinct_ids(&[]));
}

#[test]
fn name_validation() {
    assert!(service_name_is_valid("wifi"));
    assert!(service_name_is_valid("a1_b"));
    assert!(service_name_is_valid("abcdefghijklmnopqrstuvwx"));
    assert!(!service_name_is_valid("abcdefghijklmnopqrstuvwxy"));
    assert!(!service_name_is_valid(""));
    assert!(!service_name_is_valid("Wifi"));
    assert!(!service_name_is_valid("1wifi"));
    assert!(!service_name_is_valid("wi-fi"));
}

#[test]
fn invalid_declarations_do_not_compile() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/*.rs");
}
