//! The derive macros resolve the runtime crates through `rohi-sdk` when a
//! consumer depends on the umbrella crate only.
#![cfg(feature = "service")]

use minicbor::{CborLen, Decode, Encode};
use rohi_sdk::config::{ConfigSection, SectionId};
use rohi_sdk::service::Service;

#[derive(Debug, PartialEq, Encode, Decode, CborLen, ConfigSection)]
#[cbor(array)]
#[config(version = 1)]
struct Settings {
    #[n(0)]
    value: u32,
}

#[derive(Service)]
struct DemoService;

#[test]
fn derives_resolve_through_the_umbrella() {
    assert_eq!(Settings::ID, SectionId::from_name("Settings"));
    assert_eq!(Settings::VERSION, 1);
    assert_eq!(DemoService::NAME, "demo");
}
