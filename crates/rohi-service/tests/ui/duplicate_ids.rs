use rohi_service::{Service, distinct_ids};

#[derive(Service)]
struct WifiService;

#[derive(Service)]
#[service(name = "wifi")]
struct WirelessService;

const _: () = assert!(distinct_ids(&[WifiService::ID, WirelessService::ID]));

fn main() {}
