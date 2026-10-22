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

use super::*;

fn expand_str(source: &str) -> Result<String, String> {
    let input: DeriveInput = syn::parse_str(source).unwrap();
    expand(&input)
        .map(|tokens| tokens.to_string())
        .map_err(|error| error.to_string())
}

#[test]
fn fnv1a64_reference_vectors() {
    assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
}

// Changing any of these values is a breaking persistent storage change.
#[test]
fn section_id_golden_values() {
    assert_eq!(fnv1a64(b"Identity"), 0x94d6_3573_d312_fe9b);
    assert_eq!(fnv1a64(b"WifiConfig"), 0x3352_46ad_2024_8b26);
    assert_eq!(fnv1a64(b"ConnectivityConfig"), 0x9c57_015a_4066_2d0c);
}

fn id_literal(id: u64) -> String {
    quote!(#id).to_string()
}

#[test]
fn default_id_hashes_struct_name() {
    let out = expand_str("#[config(version = 3)] struct WifiConfig;").unwrap();
    assert!(out.contains(&id_literal(0x3352_46ad_2024_8b26)));
    assert!(out.contains("3u16"));
    assert!(!out.contains("NAME"));
}

#[test]
fn name_attribute_is_rejected() {
    let error =
        expand_str(r#"#[config(name = "wifi", version = 1)] struct WifiConfig;"#).unwrap_err();
    assert!(error.contains("`name` is not supported"), "{error}");
}

#[test]
fn explicit_id_is_used_verbatim() {
    let out = expand_str("#[config(id = 0xAABBCCDDEEFF0011, version = 1)] struct Legacy;").unwrap();
    assert!(out.contains(&id_literal(0xAABB_CCDD_EEFF_0011)));
}

#[test]
fn hook_is_forwarded() {
    let out = expand_str(
        r#"#[config(version = 2, decode_versioned = "migrate::wifi")] struct WifiConfig;"#,
    )
    .unwrap();
    assert!(out.contains("fn decode_versioned"));
    assert!(out.contains("migrate :: wifi (version , payload)"));
}

#[test]
fn generics_are_preserved() {
    let out = expand_str("#[config(version = 1)] struct Wrapper<T: Clone>(T);").unwrap();
    assert!(out.contains("Wrapper < T >"));
    assert!(out.contains("T : Clone"));
}

#[test]
fn missing_version_is_rejected() {
    let error = expand_str("#[config(id = 1)] struct A;").unwrap_err();
    assert!(error.contains("missing `version`"), "{error}");
    let error = expand_str("struct A;").unwrap_err();
    assert!(error.contains("missing `version`"), "{error}");
}

#[test]
fn invalid_version_is_rejected() {
    let error = expand_str("#[config(version = 70000)] struct A;").unwrap_err();
    assert!(error.contains("0..=65535"), "{error}");
    let error = expand_str(r#"#[config(version = "1")] struct A;"#).unwrap_err();
    assert!(error.contains("expected integer literal"), "{error}");
}

#[test]
fn invalid_id_is_rejected() {
    let error =
        expand_str("#[config(id = 0x1_0000_0000_0000_0000, version = 1)] struct A;").unwrap_err();
    assert!(error.contains("fits in `u64`"), "{error}");
    let error = expand_str(r#"#[config(id = "abc", version = 1)] struct A;"#).unwrap_err();
    assert!(error.contains("expected integer literal"), "{error}");
}

#[test]
fn duplicate_attributes_are_rejected() {
    let error = expand_str("#[config(version = 1, version = 2)] struct A;").unwrap_err();
    assert!(error.contains("duplicate `version`"), "{error}");
    let error =
        expand_str("#[config(version = 1)] #[config(id = 1, id = 2)] struct A;").unwrap_err();
    assert!(error.contains("duplicate `id`"), "{error}");
    let error = expand_str("#[config(version = 1)] #[config(version = 1)] struct A;").unwrap_err();
    assert!(error.contains("duplicate `version`"), "{error}");
}

#[test]
fn unknown_attribute_is_rejected() {
    let error = expand_str("#[config(version = 1, max_size = 8)] struct A;").unwrap_err();
    assert!(error.contains("unknown `config` attribute"), "{error}");
}

#[test]
fn invalid_hook_is_rejected() {
    let error = expand_str(r#"#[config(version = 1, decode_versioned = "not a path")] struct A;"#)
        .unwrap_err();
    assert!(error.contains("path to a function"), "{error}");
    let error =
        expand_str("#[config(version = 1, decode_versioned = migrate)] struct A;").unwrap_err();
    assert!(error.contains("expected string literal"), "{error}");
}
