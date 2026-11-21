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
//! The persistent `wireless` section: Wi-Fi, IP and BLE settings in one place.

use rohi_config::{Text, Variant};
use rohi_service::{Config, FieldError};

use crate::network::{
    ADDRESS_MODE_ENUM, ADDRESS_MODES, DEFAULT_DHCP_TIMEOUT_MS, DHCP_TIMEOUT_RANGE_MS, Ipv4Codec,
};
use crate::wifi::{
    BACKOFF_INITIAL_RANGE_MS, BACKOFF_MAX_RANGE_MS, CONNECT_TIMEOUT_RANGE_MS,
    DEFAULT_BACKOFF_INITIAL_MS, DEFAULT_BACKOFF_MAX_MS, DEFAULT_CONNECT_TIMEOUT_MS,
    DEFAULT_LINK_REFRESH_MS, DEFAULT_SCAN_TIMEOUT_MS, LINK_REFRESH_RANGE_MS, SCAN_TIMEOUT_RANGE_MS,
};
use crate::{
    AddressMode, BleName, BleSettings, Hostname, Ipv4, NetworkConfig, WifiConfig, WifiPassword,
    WifiSsid,
};

/// The `wireless` config section: every setting of the Wi-Fi station, its
/// IPv4 stack and the BLE peripheral, as one list of 15 fields.
///
/// The fields are the settings of three separate parts, which stay separate
/// internally: [`WifiConfig`] (`wifi_*`), [`NetworkConfig`] (`ip_*`) and
/// [`BleSettings`] (`ble_name`). [`wifi`](Self::wifi),
/// [`network`](Self::network) and [`ble`](Self::ble) return the part a
/// consumer needs.
///
/// | Id | Field | Meaning |
/// |----|-------|---------|
/// | 1 | `wifi_ssid` | Network name, 1 to 32 bytes; none disables Wi-Fi (the default) |
/// | 2 | `wifi_password` | Write-only secret, 8 to 64 bytes; none is an open network |
/// | 3 | `wifi_scan_timeout` | Bound of a scan, ms |
/// | 4 | `wifi_connect_timeout` | Bound of associating, ms |
/// | 5 | `wifi_backoff_initial` | First reconnect delay, ms |
/// | 6 | `wifi_backoff_max` | Longest reconnect delay, ms |
/// | 7 | `wifi_link_refresh` | Interval of refreshing the link details, ms |
/// | 8 | `ip_mode` | `dhcp` (the default) or `static` |
/// | 9 | `ip_address` | Static IPv4 address |
/// | 10 | `ip_prefix_len` | Prefix length of the static address, 1 to 30 |
/// | 11 | `ip_gateway` | Static gateway |
/// | 12 | `ip_dns` | Static DNS server |
/// | 13 | `ip_hostname` | Host name announced to the DHCP server |
/// | 14 | `ip_dhcp_timeout` | Bound of obtaining a DHCP lease, ms |
/// | 15 | `ble_name` | Advertised BLE name, 1 to 29 bytes; none keeps the firmware default |
///
/// The ranges, defaults and cross-field rules are those of [`WifiConfig`] and
/// [`NetworkConfig`]; the whole section is validated together, so a static
/// address needs its prefix length and gateway and the backoff start must not
/// exceed the backoff maximum. What is stored are the explicit user overrides
/// of the defaults, [`WirelessOverrides`].
///
/// ```
/// use rohi_net::{AddressMode, WirelessConfig};
///
/// // The default: no Wi-Fi network, DHCP, the firmware BLE name.
/// let config = WirelessConfig::default();
/// assert!(!config.wifi().is_configured());
/// assert_eq!(config.network().address_mode, AddressMode::Dhcp);
/// assert_eq!(config.ble().name, None);
/// ```
#[derive(Clone, PartialEq, Eq, Debug, Config)]
#[config(
    name = "wireless",
    overrides = WirelessOverrides,
    version = 1,
    validate = validate_section,
    enums = ADDRESS_MODES
)]
pub struct WirelessConfig {
    /// Network name. `None` (the default) disables Wi-Fi; a network name
    /// enables it.
    #[config(id = 1, with = Text)]
    pub wifi_ssid: Option<WifiSsid>,
    /// Passphrase, `None` for an open network.
    #[config(id = 2, secret, with = Text)]
    pub wifi_password: Option<WifiPassword>,
    /// Bound of a network scan.
    #[config(
        id = 3,
        name = "wifi_scan_timeout",
        unit = Ms,
        min = SCAN_TIMEOUT_RANGE_MS.0,
        max = SCAN_TIMEOUT_RANGE_MS.1
    )]
    pub wifi_scan_timeout_ms: u32,
    /// Bound of associating with the access point.
    #[config(
        id = 4,
        name = "wifi_connect_timeout",
        unit = Ms,
        min = CONNECT_TIMEOUT_RANGE_MS.0,
        max = CONNECT_TIMEOUT_RANGE_MS.1
    )]
    pub wifi_connect_timeout_ms: u32,
    /// First delay before a reconnect attempt, doubling up to the maximum.
    #[config(
        id = 5,
        name = "wifi_backoff_initial",
        unit = Ms,
        min = BACKOFF_INITIAL_RANGE_MS.0,
        max = BACKOFF_INITIAL_RANGE_MS.1
    )]
    pub wifi_backoff_initial_ms: u32,
    /// Longest delay between reconnect attempts.
    #[config(
        id = 6,
        name = "wifi_backoff_max",
        unit = Ms,
        min = BACKOFF_MAX_RANGE_MS.0,
        max = BACKOFF_MAX_RANGE_MS.1
    )]
    pub wifi_backoff_max_ms: u32,
    /// Interval of refreshing the connection details while connected; the
    /// link counts as stable after one such interval.
    #[config(
        id = 7,
        name = "wifi_link_refresh",
        unit = Ms,
        min = LINK_REFRESH_RANGE_MS.0,
        max = LINK_REFRESH_RANGE_MS.1
    )]
    pub wifi_link_refresh_ms: u32,
    /// How the IPv4 address is obtained.
    #[config(id = 8, with = Variant, enum_id = ADDRESS_MODE_ENUM)]
    pub ip_mode: AddressMode,
    /// Static IPv4 address.
    #[config(id = 9, with = Ipv4Codec)]
    pub ip_address: Option<Ipv4>,
    /// Length of the network prefix of the static address, 1 to 30.
    #[config(id = 10, min = 1, max = 30)]
    pub ip_prefix_len: Option<u8>,
    /// Static default gateway.
    #[config(id = 11, with = Ipv4Codec)]
    pub ip_gateway: Option<Ipv4>,
    /// Static DNS server.
    #[config(id = 12, with = Ipv4Codec)]
    pub ip_dns: Option<Ipv4>,
    /// Host name announced to the DHCP server.
    #[config(id = 13, with = Text)]
    pub ip_hostname: Option<Hostname>,
    /// Bound of obtaining an address from DHCP.
    #[config(
        id = 14,
        name = "ip_dhcp_timeout",
        unit = Ms,
        min = DHCP_TIMEOUT_RANGE_MS.0,
        max = DHCP_TIMEOUT_RANGE_MS.1
    )]
    pub ip_dhcp_timeout_ms: u32,
    /// Advertised and GAP device name. `None` keeps the firmware default; a
    /// change takes effect after a restart.
    #[config(id = 15, with = Text)]
    pub ble_name: Option<BleName>,
}

impl Default for WirelessConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl WirelessConfig {
    /// No Wi-Fi network, DHCP, the firmware BLE name and the default timing:
    /// the portable baseline, usable in constants.
    pub const fn new() -> Self {
        Self {
            wifi_ssid: None,
            wifi_password: None,
            wifi_scan_timeout_ms: DEFAULT_SCAN_TIMEOUT_MS,
            wifi_connect_timeout_ms: DEFAULT_CONNECT_TIMEOUT_MS,
            wifi_backoff_initial_ms: DEFAULT_BACKOFF_INITIAL_MS,
            wifi_backoff_max_ms: DEFAULT_BACKOFF_MAX_MS,
            wifi_link_refresh_ms: DEFAULT_LINK_REFRESH_MS,
            ip_mode: AddressMode::Dhcp,
            ip_address: None,
            ip_prefix_len: None,
            ip_gateway: None,
            ip_dns: None,
            ip_hostname: None,
            ip_dhcp_timeout_ms: DEFAULT_DHCP_TIMEOUT_MS,
            ble_name: None,
        }
    }

    /// The Wi-Fi part, which the Wi-Fi manager consumes.
    pub fn wifi(&self) -> WifiConfig {
        WifiConfig {
            ssid: self.wifi_ssid.clone(),
            password: self.wifi_password.clone(),
            scan_timeout_ms: self.wifi_scan_timeout_ms,
            connect_timeout_ms: self.wifi_connect_timeout_ms,
            backoff_initial_ms: self.wifi_backoff_initial_ms,
            backoff_max_ms: self.wifi_backoff_max_ms,
            link_refresh_ms: self.wifi_link_refresh_ms,
        }
    }

    /// The IPv4 part, which the Wi-Fi manager consumes.
    pub fn network(&self) -> NetworkConfig {
        NetworkConfig {
            address_mode: self.ip_mode,
            static_ip: self.ip_address,
            prefix_len: self.ip_prefix_len,
            gateway: self.ip_gateway,
            dns: self.ip_dns,
            hostname: self.ip_hostname.clone(),
            dhcp_timeout_ms: self.ip_dhcp_timeout_ms,
        }
    }

    /// The BLE part, which the peripheral reads when it starts.
    pub fn ble(&self) -> BleSettings {
        BleSettings {
            name: self.ble_name.clone(),
        }
    }
}

/// A section without an SSID is valid: Wi-Fi is disabled, the radio stays
/// idle. The credentials, the timing and the IPv4 settings are checked as a
/// whole.
fn validate_section(config: &WirelessConfig) -> Result<(), FieldError> {
    config
        .wifi()
        .validate_settings()
        .and_then(|()| config.network().validate())
        .map_err(|_| FieldError::Invalid)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::net::Ipv4Addr;

    use rohi_config::{
        ConfigOverrides, ConfigSection, ConfigStore, MemoryStorage, Override, SectionId,
    };
    use rohi_service::{MAX_CONFIG_FIELDS, MAX_ENUMS, Value};

    use super::*;
    use crate::{MAX_SSID_LEN, MAX_WIFI_PASSWORD_LEN, address_mode};

    const SSID: u32 = 1;
    const PASSWORD: u32 = 2;
    const SCAN_TIMEOUT: u32 = 3;
    const CONNECT_TIMEOUT: u32 = 4;
    const BACKOFF_INITIAL: u32 = 5;
    const BACKOFF_MAX: u32 = 6;
    const LINK_REFRESH: u32 = 7;
    const IP_MODE: u32 = 8;
    const IP_ADDRESS: u32 = 9;
    const IP_PREFIX_LEN: u32 = 10;
    const IP_GATEWAY: u32 = 11;
    const IP_DNS: u32 = 12;
    const IP_HOSTNAME: u32 = 13;
    const IP_DHCP_TIMEOUT: u32 = 14;
    const BLE_NAME: u32 = 15;

    fn text(value: &str) -> Value {
        Value::text(value).unwrap()
    }

    fn encode(overrides: &WirelessOverrides) -> std::vec::Vec<u8> {
        let mut buffer = [0u8; 512];
        let used = rohi_config::encode_to_slice(overrides, &mut buffer).unwrap();
        assert_eq!(used, minicbor::len(overrides));
        buffer[..used].to_vec()
    }

    fn roundtrip(overrides: &WirelessOverrides) -> WirelessOverrides {
        rohi_config::decode_exact(&encode(overrides)).unwrap()
    }

    #[test]
    fn the_section_declares_the_fifteen_agreed_fields() {
        let fields: std::vec::Vec<_> = WirelessConfig::FIELDS
            .iter()
            .map(|field| (field.field.id, field.field.name))
            .collect();
        assert_eq!(
            fields,
            [
                (1, "wifi_ssid"),
                (2, "wifi_password"),
                (3, "wifi_scan_timeout"),
                (4, "wifi_connect_timeout"),
                (5, "wifi_backoff_initial"),
                (6, "wifi_backoff_max"),
                (7, "wifi_link_refresh"),
                (8, "ip_mode"),
                (9, "ip_address"),
                (10, "ip_prefix_len"),
                (11, "ip_gateway"),
                (12, "ip_dns"),
                (13, "ip_hostname"),
                (14, "ip_dhcp_timeout"),
                (15, "ble_name"),
            ]
        );
        assert_eq!(<WirelessConfig as Config>::NAME, "wireless");
    }

    #[test]
    fn the_schema_fits_the_limits_and_every_field_can_be_overridden() {
        assert!(WirelessConfig::FIELDS.len() <= MAX_CONFIG_FIELDS);
        assert!(WirelessConfig::ENUMS.len() <= MAX_ENUMS);
        for field in WirelessConfig::FIELDS {
            assert!(field.field.name.len() <= 24, "{}", field.field.name);
        }
        let mut ids: std::vec::Vec<_> = WirelessConfig::FIELDS.iter().map(|f| f.field.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, WirelessOverrides::FIELD_IDS);
    }

    #[test]
    fn the_password_is_the_only_secret_field() {
        let secret: std::vec::Vec<_> = WirelessConfig::FIELDS
            .iter()
            .filter(|field| field.secret)
            .map(|field| field.field.name)
            .collect();
        assert_eq!(secret, ["wifi_password"]);
    }

    #[test]
    fn the_section_id_is_derived_from_the_type_name() {
        assert_eq!(
            WirelessOverrides::ID,
            SectionId::from_name("WirelessConfig")
        );
        assert_eq!(<WirelessOverrides as ConfigSection>::VERSION, 1);
    }

    #[test]
    fn the_defaults_are_the_defaults_of_the_parts() {
        let config = WirelessConfig::default();
        assert_eq!(config.wifi(), WifiConfig::default());
        assert_eq!(config.network(), NetworkConfig::default());
        assert_eq!(config.ble(), BleSettings::default());
        assert_eq!(Config::validate(&config), Ok(()));
        let read = |id| config.read_field(id).unwrap();
        assert_eq!(read(SCAN_TIMEOUT), Some(Value::U64(20_000)));
        assert_eq!(read(CONNECT_TIMEOUT), Some(Value::U64(30_000)));
        assert_eq!(read(BACKOFF_INITIAL), Some(Value::U64(1_000)));
        assert_eq!(read(BACKOFF_MAX), Some(Value::U64(60_000)));
        assert_eq!(read(LINK_REFRESH), Some(Value::U64(10_000)));
        assert_eq!(read(IP_MODE), Some(Value::Enum(address_mode::DHCP)));
        assert_eq!(read(IP_DHCP_TIMEOUT), Some(Value::U64(20_000)));
        for id in [
            SSID,
            IP_ADDRESS,
            IP_PREFIX_LEN,
            IP_GATEWAY,
            IP_DNS,
            IP_HOSTNAME,
            BLE_NAME,
        ] {
            assert_eq!(read(id), None);
        }
    }

    #[test]
    fn nothing_is_stored_for_the_defaults() {
        let overrides = WirelessOverrides::default();
        assert!(overrides.is_empty());
        assert_eq!(encode(&overrides), [0xa0]);
        assert_eq!(
            rohi_config::materialize::<WirelessConfig>(&Default::default(), &overrides),
            WirelessConfig::default()
        );
    }

    #[test]
    fn the_cbor_encoding_is_stable() {
        let overrides = WirelessOverrides {
            wifi_ssid: Override::Value(WifiSsid::from_bytes(b"ab")),
            ip_mode: Override::Value(AddressMode::Static),
            ble_name: Override::Value(BleName::new("urban")),
            ..WirelessOverrides::default()
        };
        assert_eq!(
            encode(&overrides),
            [
                0xa3, // map(3)
                0x01, 0x42, 0x61, 0x62, // 1: wifi_ssid
                0x08, 0x01, // 8: ip_mode static
                0x0f, 0x65, b'u', b'r', b'b', b'a', b'n', // 15: ble_name
            ]
        );
        assert_eq!(roundtrip(&overrides), overrides);
    }

    #[test]
    fn other_layouts_and_trailing_bytes_are_rejected() {
        assert!(rohi_config::decode_exact::<WirelessOverrides>(&[0x81, 0x00]).is_err());
        let mut bytes = encode(&WirelessOverrides::default());
        bytes.push(0x00);
        assert!(rohi_config::decode_exact::<WirelessOverrides>(&bytes).is_err());
        // A stored name longer than the limit must not panic.
        let mut long = std::vec![0xa1, 0x0f, 0x78, 30];
        long.extend([b'a'; 30]);
        assert!(rohi_config::decode_exact::<WirelessOverrides>(&long).is_err());
    }

    #[test]
    fn the_largest_value_fits_a_section() {
        let overrides = WirelessOverrides {
            wifi_ssid: Override::Value(WifiSsid::from_bytes(&[b'a'; MAX_SSID_LEN])),
            wifi_password: Override::Value(WifiPassword::from_bytes(
                &[b'p'; MAX_WIFI_PASSWORD_LEN],
            )),
            wifi_scan_timeout_ms: Override::Value(u32::MAX),
            wifi_connect_timeout_ms: Override::Value(u32::MAX),
            wifi_backoff_initial_ms: Override::Value(u32::MAX),
            wifi_backoff_max_ms: Override::Value(u32::MAX),
            wifi_link_refresh_ms: Override::Value(u32::MAX),
            ip_mode: Override::Value(AddressMode::Static),
            ip_address: Override::Value(Some(Ipv4Addr::new(255, 255, 255, 254).into())),
            ip_prefix_len: Override::Value(Some(30)),
            ip_gateway: Override::Value(Some(Ipv4Addr::new(255, 255, 255, 253).into())),
            ip_dns: Override::Value(Some(Ipv4Addr::new(255, 255, 255, 255).into())),
            ip_hostname: Override::Value(Hostname::new(&"h".repeat(crate::MAX_HOSTNAME_LEN))),
            ip_dhcp_timeout_ms: Override::Value(u32::MAX),
            ble_name: Override::Value(BleName::new(&"b".repeat(crate::MAX_BLE_NAME_LEN))),
        };
        assert!(minicbor::len(&overrides) <= rohi_config::MAX_SECTION_PAYLOAD);
        assert_eq!(roundtrip(&overrides), overrides);
    }

    #[test]
    fn overrides_persist_in_the_config_store_and_can_be_removed() {
        let mut store = ConfigStore::open(MemoryStorage::<16384>::new()).unwrap();
        let overrides = WirelessOverrides {
            wifi_ssid: Override::Value(WifiSsid::from_bytes(b"office")),
            wifi_password: Override::Value(WifiPassword::from_bytes(b"hunter2hunter2")),
            ..WirelessOverrides::default()
        };
        store.set(&overrides).unwrap();
        assert_eq!(store.get::<WirelessOverrides>().unwrap(), Some(overrides));
        store.remove::<WirelessOverrides>().unwrap();
        assert_eq!(store.get::<WirelessOverrides>().unwrap(), None);
    }

    #[test]
    fn the_password_is_never_converted_to_a_value() {
        let mut config = WirelessConfig::default();
        config.set_field(PASSWORD, text("hunter2hunter2")).unwrap();
        assert_eq!(config.read_field(PASSWORD), Err(FieldError::Secret));
        assert_eq!(config.field_is_set(PASSWORD), Ok(true));
        assert_eq!(
            config.wifi().password,
            WifiPassword::from_bytes(b"hunter2hunter2")
        );
        let mut overrides = WirelessOverrides::default();
        assert!(overrides.capture(&config, PASSWORD));
        for shown in [
            std::format!("{config:?}"),
            std::format!("{config:#?}"),
            std::format!("{overrides:?}"),
            std::format!("{overrides:#?}"),
        ] {
            assert!(!shown.contains("hunter2"), "{shown}");
            assert!(shown.contains("redacted"), "{shown}");
        }
    }

    #[test]
    fn the_password_is_optional_and_clearing_it_restores_an_open_network() {
        let mut config = WirelessConfig::default();
        assert_eq!(config.field_is_set(PASSWORD), Ok(false));
        config.set_field(PASSWORD, text("hunter2hunter2")).unwrap();
        let mut overrides = WirelessOverrides::default();
        assert!(overrides.capture(&config, PASSWORD));
        assert!(overrides.clear(PASSWORD));
        assert_eq!(
            rohi_config::materialize::<WirelessConfig>(&Default::default(), &overrides)
                .wifi_password,
            None
        );
        assert_eq!(config.set_none(PASSWORD), Ok(()));
        assert_eq!(config.wifi_password, None);
    }

    #[test]
    fn none_is_only_for_the_optional_fields() {
        let mut config = WirelessConfig::default();
        for id in [
            SCAN_TIMEOUT,
            CONNECT_TIMEOUT,
            BACKOFF_INITIAL,
            BACKOFF_MAX,
            LINK_REFRESH,
            IP_MODE,
            IP_DHCP_TIMEOUT,
        ] {
            assert_eq!(config.set_none(id), Err(FieldError::NotOptional), "{id}");
        }
        for id in [
            SSID,
            PASSWORD,
            IP_ADDRESS,
            IP_PREFIX_LEN,
            IP_GATEWAY,
            IP_DNS,
            IP_HOSTNAME,
            BLE_NAME,
        ] {
            assert_eq!(config.set_none(id), Ok(()), "{id}");
        }
        assert_eq!(config.set_none(99), Err(FieldError::UnknownField));
    }

    #[test]
    fn no_ssid_disables_wifi_and_an_ssid_enables_it() {
        let mut config = WirelessConfig::default();
        assert!(!config.wifi().is_configured());
        config.set_field(SSID, text("office")).unwrap();
        assert!(config.wifi().is_configured());
        // An empty name is not a network: `none` disables Wi-Fi instead.
        assert_eq!(config.set_field(SSID, text("")), Err(FieldError::Invalid));
        assert_eq!(config.set_none(SSID), Ok(()));
        assert_eq!(config.read_field(SSID), Ok(None));
        assert_eq!(Config::validate(&config), Ok(()));
    }

    #[test]
    fn an_explicit_no_ssid_overrides_a_firmware_default_network() {
        let firmware = WirelessConfig {
            wifi_ssid: WifiSsid::from_bytes(b"factory"),
            ..WirelessConfig::default()
        };
        let mut config = firmware.clone();
        config.set_none(SSID).unwrap();
        let mut overrides = WirelessOverrides::default();
        assert!(overrides.capture(&config, SSID));
        assert_eq!(overrides.wifi_ssid, Override::Value(None));
        let mut effective = firmware.clone();
        overrides.apply_to(&mut effective);
        assert!(!effective.wifi().is_configured());
        overrides.clear(SSID);
        let mut effective = firmware.clone();
        overrides.apply_to(&mut effective);
        assert_eq!(effective.wifi_ssid, firmware.wifi_ssid);
    }

    #[test]
    fn the_timing_belongs_to_the_whole_section() {
        let mut config = WirelessConfig::default();
        config.set_field(BACKOFF_MAX, Value::U64(500)).unwrap();
        // Lower than the initial delay of 1 s.
        assert_eq!(Config::validate(&config), Err(FieldError::Invalid));
        config.set_field(BACKOFF_INITIAL, Value::U64(200)).unwrap();
        assert_eq!(Config::validate(&config), Ok(()));
    }

    #[test]
    fn a_short_password_makes_the_section_invalid() {
        let mut config = WirelessConfig::default();
        config.set_field(SSID, text("office")).unwrap();
        config.set_field(PASSWORD, text("short")).unwrap();
        assert_eq!(Config::validate(&config), Err(FieldError::Invalid));
    }

    #[test]
    fn a_static_address_is_set_field_by_field_and_validated_as_a_whole() {
        let mut config = WirelessConfig::default();
        config
            .set_field(IP_MODE, Value::Enum(address_mode::STATIC))
            .unwrap();
        // Static mode without an address is incomplete.
        assert_eq!(Config::validate(&config), Err(FieldError::Invalid));

        config.set_field(IP_ADDRESS, text("192.168.1.50")).unwrap();
        config.set_field(IP_PREFIX_LEN, Value::U64(24)).unwrap();
        config.set_field(IP_GATEWAY, text("192.168.1.1")).unwrap();
        config.set_field(IP_DNS, text("1.1.1.1")).unwrap();
        assert_eq!(Config::validate(&config), Ok(()));
        assert_eq!(
            config.read_field(IP_ADDRESS),
            Ok(Some(text("192.168.1.50")))
        );
        assert_eq!(config.read_field(IP_PREFIX_LEN), Ok(Some(Value::U64(24))));
        assert_eq!(config.read_field(IP_DNS), Ok(Some(text("1.1.1.1"))));
        assert!(config.network().static_settings().is_some());

        // A gateway outside the network is rejected by the whole section.
        config.set_field(IP_GATEWAY, text("10.0.0.1")).unwrap();
        assert_eq!(Config::validate(&config), Err(FieldError::Invalid));
    }

    #[test]
    fn bad_values_are_refused() {
        let mut config = WirelessConfig::default();
        let cases: [(u32, Value, FieldError); 9] = [
            (SSID, Value::U64(1), FieldError::WrongKind),
            (SSID, text(&"a".repeat(33)), FieldError::Invalid),
            (BACKOFF_MAX, Value::U64(u64::MAX), FieldError::OutOfRange),
            (IP_ADDRESS, text("not an address"), FieldError::Invalid),
            (IP_ADDRESS, text("192.168.1.256"), FieldError::Invalid),
            (IP_PREFIX_LEN, Value::U64(300), FieldError::OutOfRange),
            (IP_MODE, Value::Enum(7), FieldError::OutOfRange),
            (IP_HOSTNAME, text("bad name"), FieldError::Invalid),
            (BLE_NAME, text(&"a".repeat(30)), FieldError::Invalid),
        ];
        for (id, value, error) in cases {
            assert_eq!(config.set_field(id, value), Err(error), "{id}");
        }
        assert_eq!(
            config.set_field(BLE_NAME, text("")),
            Err(FieldError::Invalid)
        );
        assert_eq!(
            config.set_field(99, Value::U64(1)),
            Err(FieldError::UnknownField)
        );
    }

    #[test]
    fn the_ble_name_is_optional_and_validated() {
        let mut config = WirelessConfig::default();
        config.set_field(BLE_NAME, text("urban")).unwrap();
        assert_eq!(config.read_field(BLE_NAME), Ok(Some(text("urban"))));
        assert_eq!(config.ble().name, BleName::new("urban"));
        assert_eq!(Config::validate(&config), Ok(()));
        let mut overrides = WirelessOverrides::default();
        assert!(overrides.capture(&config, BLE_NAME));
        assert!(overrides.clear(BLE_NAME));
        assert_eq!(
            rohi_config::materialize::<WirelessConfig>(&Default::default(), &overrides).ble_name,
            None
        );
    }

    #[test]
    fn the_parts_carry_the_fields_that_belong_to_them() {
        let mut config = WirelessConfig::default();
        config.set_field(SSID, text("office")).unwrap();
        config.set_field(SCAN_TIMEOUT, Value::U64(5_000)).unwrap();
        config.set_field(IP_HOSTNAME, text("altruist-1")).unwrap();
        config
            .set_field(IP_DHCP_TIMEOUT, Value::U64(5_000))
            .unwrap();
        config.set_field(BLE_NAME, text("urban")).unwrap();
        let wifi = config.wifi();
        assert_eq!(wifi.ssid, WifiSsid::from_bytes(b"office"));
        assert_eq!(wifi.scan_timeout_ms, 5_000);
        let network = config.network();
        assert_eq!(network.hostname, Hostname::new("altruist-1"));
        assert_eq!(network.dhcp_timeout_ms, 5_000);
        assert_eq!(config.ble().name, BleName::new("urban"));
    }
}
