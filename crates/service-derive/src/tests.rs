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

use super::*;

fn expand_str(source: &str) -> Result<String, String> {
    let input: DeriveInput = syn::parse_str(source).unwrap();
    expand(&input)
        .map(|tokens| tokens.to_string())
        .map_err(|error| error.to_string())
}

#[test]
fn default_name_rule() {
    for (type_name, name) in [
        ("WifiService", "wifi"),
        ("BleService", "ble"),
        ("TimeSyncService", "time_sync"),
        ("BLEService", "ble"),
        ("HTTPServerService", "http_server"),
        ("Wifi", "wifi"),
        ("Service", "service"),
        ("Wifi6Service", "wifi6"),
        ("Wifi6AxService", "wifi6_ax"),
        ("MyServiceService", "my_service"),
    ] {
        assert_eq!(default_name(type_name), name, "{type_name}");
    }
}

#[test]
fn derives_name_and_id() {
    let out = expand_str("struct WifiService;").unwrap();
    assert!(out.contains("\"wifi\""));
    assert!(out.contains("service_id (\"wifi\")"));
}

#[test]
fn explicit_name_replaces_the_rule() {
    let out = expand_str(r#"#[service(name = "wifi")] struct WirelessService;"#).unwrap();
    assert!(out.contains("\"wifi\""));
    assert!(!out.contains("wireless"));
}

#[test]
fn generics_are_preserved() {
    let out = expand_str("struct WifiService<'a, M: Clone>(&'a M);").unwrap();
    assert!(out.contains("impl < 'a , M : Clone >"));
    assert!(out.contains("for WifiService < 'a , M >"));
}

#[test]
fn rejects_invalid_names() {
    for name in ["", "Wifi", "1wifi", "wi-fi", "wi fi"] {
        let error = expand_str(&format!(r#"#[service(name = "{name}")] struct S;"#)).unwrap_err();
        assert!(error.contains("invalid service name"), "{name}: {error}");
    }
}

#[test]
fn rejects_unknown_and_duplicate_keys() {
    let error = expand_str(r#"#[service(id = 1)] struct S;"#).unwrap_err();
    assert!(error.contains("unknown `service` key"), "{error}");
    let error = expand_str(r#"#[service(name = "a", name = "b")] struct S;"#).unwrap_err();
    assert!(error.contains("duplicate"), "{error}");
}

#[test]
fn rejects_unions() {
    let error = expand_str("union U { a: u32 }").unwrap_err();
    assert!(error.contains("struct or an enum"), "{error}");
}
