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
    let out = expand_str("#[config(id = 7, version = 1)] struct Wrapper<T: Clone>(T);").unwrap();
    assert!(out.contains("Wrapper < T >"));
    assert!(out.contains("T : Clone"));
    let out = expand_str("#[config(version = 1)] struct Borrowed<'a>(&'a u8);").unwrap();
    assert!(out.contains(&id_literal(fnv1a64(b"Borrowed"))));
}

#[test]
fn generic_types_need_an_explicit_id() {
    let error = expand_str("#[config(version = 1)] struct Wrapper<T>(T);").unwrap_err();
    assert!(error.contains("explicit `id"), "{error}");
    let error = expand_str("#[config(version = 1)] struct Buf<const N: usize>;").unwrap_err();
    assert!(error.contains("explicit `id"), "{error}");
}

#[test]
fn raw_identifiers_hash_without_prefix() {
    let out = expand_str("#[config(version = 1)] struct r#WifiConfig;").unwrap();
    assert!(out.contains(&id_literal(0x3352_46ad_2024_8b26)));
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

fn config_str(source: &str) -> Result<String, String> {
    let input: DeriveInput = syn::parse_str(source).unwrap();
    expand_config(&input)
        .map(|tokens| tokens.to_string())
        .map_err(|error| error.to_string())
}

const ATTR: &str = r#"#[config(name = "demo", overrides = O, version = 1)]"#;

fn with_fields(fields: &str) -> String {
    format!("{ATTR} struct A {{ {fields} }}")
}

const WIFI: &str = r#"#[config(name = "wifi", overrides = WifiOverrides, version = 1)]
    pub struct WifiConfig {
        #[config(id = 1, with = SsidCodec)] pub ssid: Option<Ssid>,
        #[config(id = 3, name = "scan_timeout", unit = Ms, min = 1000, max = 9000)] pub scan: u32,
    }"#;

#[test]
fn overrides_are_generated_from_the_config() {
    let out = config_str(WIFI).unwrap();
    assert!(out.contains("pub struct WifiOverrides"));
    assert!(out.contains("Override < Option < Ssid > >"));
    assert!(out.contains("# [n (3u32)]"));
    assert!(out.contains("HasOverrides for WifiConfig"));
    assert!(out.contains("ConfigOverrides < WifiConfig > for WifiOverrides"));
    assert!(out.contains("1u16"));
}

#[test]
fn the_overrides_section_id_hashes_the_config_name() {
    let out = config_str(WIFI).unwrap();
    assert!(out.contains(&id_literal(0x3352_46ad_2024_8b26)));
    assert!(!out.contains(&id_literal(fnv1a64(b"WifiOverrides"))));
}

#[test]
fn overrides_explicit_id_is_used_verbatim() {
    let out = config_str(
        r#"#[config(name = "r", overrides = O, version = 1, id = 0xAABBCCDDEEFF0011)]
         struct Renamed { #[config(id = 1)] a: u8 }"#,
    )
    .unwrap();
    assert!(out.contains(&id_literal(0xAABB_CCDD_EEFF_0011)));
}

#[test]
fn the_config_implementation_is_generated_from_the_same_declaration() {
    let out = config_str(WIFI).unwrap();
    assert!(out.contains("Config for WifiConfig"));
    assert!(out.contains("const NAME : & 'static str = \"wifi\""));
    // The public name of a field defaults to its identifier.
    assert!(out.contains("ConfigField :: new (1u32 , \"ssid\""));
    assert!(out.contains("ConfigField :: new (3u32 , \"scan_timeout\""));
    assert!(out.contains("Unit :: Ms"));
    assert!(out.contains("Bounds :: U64"));
    // An `Option` field is optional, the codec converts the inner type.
    assert!(out.contains(". optional ()"));
    assert!(out.contains("< SsidCodec as :: rohi_service :: Codec < Ssid >> :: KIND"));
    assert!(out.contains("< :: rohi_service :: Standard as :: rohi_service :: Codec < u32 >>"));
    // Without `validate` every section is valid.
    assert!(out.contains("fn validate"));
}

#[test]
fn the_validation_hook_is_invoked() {
    let out = config_str(
        r#"#[config(name = "a", overrides = O, version = 1, validate = check::all)]
        struct A { #[config(id = 1)] a: u8 }"#,
    )
    .unwrap();
    assert!(out.contains("check :: all (self)"));
    let out = config_str(
        r#"#[config(name = "a", overrides = O, version = 1, validate = "check")]
        struct A { #[config(id = 1)] a: u8 }"#,
    )
    .unwrap();
    assert!(out.contains("check (self)"));
    let error = config_str(
        r#"#[config(name = "a", overrides = O, version = 1, validate = "not a path")]
        struct A { #[config(id = 1)] a: u8 }"#,
    )
    .unwrap_err();
    assert!(error.contains("path to a function"), "{error}");
}

#[test]
fn a_secret_is_never_read_and_redacted_in_debug() {
    let out = config_str(&with_fields(
        "#[config(id = 1, secret)] token: Option<Token>,",
    ))
    .unwrap();
    assert!(out.contains("FieldError :: Secret"));
    assert!(out.contains("RedactedOverride"));
    assert!(out.contains("self . token . is_some ()"));
    assert!(out.contains(". secret ()"));
    // The copy handed to the setter is wiped.
    assert!(out.contains("value . wipe ()"));
    // The generated `Debug` replaces the derived one.
    assert!(!out.contains("Debug ,"));
}

#[test]
fn a_read_only_field_is_exposed_but_not_persisted() {
    let out = config_str(&with_fields(
        "#[config(id = 1)] a: u8, #[config(id = 2, read_only)] serial: u8,",
    ))
    .unwrap();
    assert!(out.contains("FIELD_IDS : & 'static [u32] = & [1u32]"));
    assert!(out.contains("pub a : :: rohi_config :: Override < u8 >"));
    assert!(!out.contains("pub serial"));
    assert!(out.contains(". read_only ()"));
    assert!(out.contains("FieldError :: ReadOnly"));
}

#[test]
fn required_keeps_an_option_field_required() {
    let optional = config_str(&with_fields("#[config(id = 1)] a: Option<u8>,")).unwrap();
    assert!(optional.contains(". optional ()"));
    let required = config_str(&with_fields("#[config(id = 1, required)] a: Option<u8>,")).unwrap();
    assert!(!required.contains(". optional ()"));
    assert!(required.contains("FieldError :: NotOptional"));
    let plain = config_str(&with_fields("#[config(id = 1)] a: u8,")).unwrap();
    assert!(!plain.contains(". optional ()"));
}

#[test]
fn enums_and_enum_ids_are_forwarded() {
    let out = config_str(
        r#"#[config(name = "a", overrides = O, version = 1, enums = MODES)]
        struct A { #[config(id = 1, with = ModeCodec, enum_id = 1)] mode: Mode }"#,
    )
    .unwrap();
    assert!(out.contains("const ENUMS : & 'static [:: rohi_service :: EnumDescriptor] = MODES"));
    assert!(out.contains(". with_enum (1)"));
}

#[test]
fn a_config_needs_name_overrides_version_and_field_ids() {
    let error =
        config_str("#[config(overrides = O, version = 1)] struct A { #[config(id = 1)] a: u8 }")
            .unwrap_err();
    assert!(error.contains("missing `name`"), "{error}");
    let error =
        config_str(r#"#[config(name = "a", version = 1)] struct A { #[config(id = 1)] a: u8 }"#)
            .unwrap_err();
    assert!(error.contains("missing `overrides`"), "{error}");
    let error =
        config_str(r#"#[config(name = "a", overrides = O)] struct A { #[config(id = 1)] a: u8 }"#)
            .unwrap_err();
    assert!(error.contains("missing `version`"), "{error}");
    let error = config_str(&with_fields("a: u8,")).unwrap_err();
    assert!(error.contains("needs a stable id"), "{error}");
}

#[test]
fn names_are_checked() {
    for name in ["", "Wifi", "1a", "a-b", "a b"] {
        let error = config_str(&format!(
            r#"#[config(name = "{name}", overrides = O, version = 1)] struct A {{ #[config(id = 1)] a: u8 }}"#
        ))
        .unwrap_err();
        assert!(error.contains("invalid section name"), "{name}: {error}");
        let error = config_str(&with_fields(&format!(
            r#"#[config(id = 1, name = "{name}")] a: u8,"#
        )))
        .unwrap_err();
        assert!(error.contains("invalid field name"), "{name}: {error}");
    }
}

#[test]
fn duplicate_ids_and_names_are_rejected() {
    let error = config_str(&with_fields(
        "#[config(id = 1)] a: u8, #[config(id = 1)] b: u8,",
    ))
    .unwrap_err();
    assert!(error.contains("duplicate field id 1"), "{error}");
    let error = config_str(&with_fields(
        r#"#[config(id = 1)] a: u8, #[config(id = 2, name = "a")] b: u8,"#,
    ))
    .unwrap_err();
    assert!(error.contains("duplicate field name `a`"), "{error}");
}

#[test]
fn contradicting_flags_are_rejected() {
    let error = config_str(&with_fields("#[config(id = 1, secret)] a: String,")).unwrap_err();
    assert!(error.contains("must be an `Option`"), "{error}");
    let error = config_str(&with_fields(
        "#[config(id = 1, secret, read_only)] a: Option<String>,",
    ))
    .unwrap_err();
    assert!(error.contains("both `secret` and `read_only`"), "{error}");
    let error = config_str(&with_fields("#[config(id = 1, required)] a: u8,")).unwrap_err();
    assert!(error.contains("only applies to an `Option`"), "{error}");
    let error = config_str(&with_fields(
        "#[config(id = 1, required, read_only)] a: Option<u8>,",
    ))
    .unwrap_err();
    assert!(error.contains("`read_only`"), "{error}");
    let error = config_str(&with_fields("#[config(id = 1, min = 1)] a: u8,")).unwrap_err();
    assert!(error.contains("both `min` and `max`"), "{error}");
    let error = config_str(&with_fields(
        "#[config(id = 1, secret = true)] a: Option<u8>,",
    ))
    .unwrap_err();
    assert!(error.contains("takes no value"), "{error}");
}

#[test]
fn overrides_reject_unsupported_shapes() {
    for source in [
        format!("{ATTR} struct A(u8);"),
        format!("{ATTR} struct A;"),
        format!("{ATTR} struct A {{}}"),
        format!("{ATTR} enum A {{ B }}"),
        format!("{ATTR} struct A<T> {{ #[config(id = 1)] a: T }}"),
        format!("{ATTR} struct A<'a> {{ #[config(id = 1)] a: &'a u8 }}"),
    ] {
        assert!(config_str(&source).is_err(), "{source}");
    }
}

#[test]
fn unknown_and_duplicate_attributes_are_rejected() {
    let error = config_str(
        r#"#[config(name = "a", overrides = O, version = 1, max_size = 8)] struct A { #[config(id = 1)] a: u8 }"#,
    )
    .unwrap_err();
    assert!(error.contains("unknown `config` attribute"), "{error}");
    let error = config_str(
        r#"#[config(name = "a", overrides = O, overrides = P, version = 1)] struct A { #[config(id = 1)] a: u8 }"#,
    )
    .unwrap_err();
    assert!(error.contains("duplicate `overrides`"), "{error}");
    let error = config_str(&with_fields("#[config(id = 1, size = 2)] a: u8,")).unwrap_err();
    assert!(
        error.contains("unknown field `config` attribute"),
        "{error}"
    );
    let error = config_str(&with_fields("#[config(id = 1, id = 2)] a: u8,")).unwrap_err();
    assert!(error.contains("duplicate `id`"), "{error}");
    let error = config_str(&with_fields("#[config(id = 4294967296)] a: u8,")).unwrap_err();
    assert!(error.contains("fits in `u32`"), "{error}");
}
