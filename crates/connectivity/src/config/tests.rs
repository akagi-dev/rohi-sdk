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
use crate::endpoint::MAX_ENDPOINT_LEN;

// More slots than a device keeps, to cover every edge; the firmware uses 2.
type Set = Routes<3>;

fn set() -> Set {
    Set::default()
}

#[test]
fn there_is_no_route_until_one_is_set_and_the_defaults_are_documented() {
    let mut set = set();
    assert_eq!(set.count(), 0);
    assert!((0..3).all(|index| set.get(index).is_none()));
    set.set_endpoint(1, 0, Some("http://ingress.example.org"))
        .unwrap();
    assert_eq!(set.count(), 1);
    assert!(set.get(0).is_none() && set.get(2).is_none());
    let route = set.get(1).unwrap();
    assert_eq!(route.interval_s, DEFAULT_INTERVAL_S);
    assert_eq!(route.interval_s, 60);
    assert_eq!(route.fallback, None);
    assert_eq!(route.profile_index, 0);
}

#[test]
fn indices_are_bounded_by_the_capacity() {
    let mut set = set();
    for index in [0, 1, 2] {
        set.set_endpoint(index, 0, Some("http://a.example"))
            .unwrap();
    }
    for index in [3, 4, usize::MAX] {
        let before = set.clone();
        assert_eq!(
            set.set_endpoint(index, 0, Some("http://a.example")),
            Err(RouteError::InvalidIndex)
        );
        assert_eq!(
            set.set_endpoint(index, 0, None),
            Err(RouteError::InvalidIndex)
        );
        assert_eq!(set.set_interval(index, 60), Err(RouteError::InvalidIndex));
        assert_eq!(
            set.set_fallback(index, Some("http://b.example")),
            Err(RouteError::InvalidIndex)
        );
        assert_eq!(set, before);
        assert!(set.get(index).is_none());
    }
}

#[test]
fn endpoints_and_intervals_are_validated() {
    let mut set = set();
    for bad in [
        "",
        "https://a.example",
        "http://a.example/x",
        "a.example",
        "http://",
    ] {
        assert_eq!(
            set.set_endpoint(0, 0, Some(bad)),
            Err(RouteError::InvalidEndpoint),
            "{bad:?}"
        );
    }
    assert_eq!(set, Set::EMPTY);
    set.set_endpoint(0, 0, Some("http://a.example")).unwrap();
    for bad in [0, MIN_INTERVAL_S - 1, MAX_INTERVAL_S + 1, u32::MAX] {
        assert_eq!(set.set_interval(0, bad), Err(RouteError::InvalidInterval));
    }
    set.set_interval(0, MIN_INTERVAL_S).unwrap();
    set.set_interval(0, MAX_INTERVAL_S).unwrap();
    assert_eq!(set.get(0).unwrap().interval_s, MAX_INTERVAL_S);
    assert_eq!(
        set.set_fallback(0, Some("http://a.example/x")),
        Err(RouteError::InvalidEndpoint)
    );
}

#[test]
fn an_absent_endpoint_deletes_the_route_and_an_empty_slot_stays_empty() {
    let mut set = set();
    set.set_endpoint(0, 1, Some("http://a.example")).unwrap();
    set.set_endpoint(1, 0, Some("http://b.example")).unwrap();
    let generation = set.generation;
    set.set_endpoint(0, 0, None).unwrap();
    assert!(set.get(0).is_none());
    assert_eq!(set.get(1).unwrap().endpoint.as_str(), "http://b.example");
    assert!(set.generation > generation);

    // Deleting an empty slot is not an error and changes nothing.
    let before = set.clone();
    set.set_endpoint(0, 0, None).unwrap();
    set.set_endpoint(2, 0, None).unwrap();
    assert_eq!(set, before);
}

#[test]
fn set_interval_and_set_fallback_need_an_existing_route() {
    let mut set = set();
    assert_eq!(set.set_interval(0, 60), Err(RouteError::RouteNotFound));
    assert_eq!(
        set.set_fallback(0, Some("http://b.example")),
        Err(RouteError::RouteNotFound)
    );
    assert_eq!(set.set_fallback(0, None), Err(RouteError::RouteNotFound));
    assert_eq!(set, Set::EMPTY);
}

#[test]
fn replacing_the_endpoint_or_profile_is_a_new_generation_and_keeps_the_rest() {
    let mut set = set();
    set.set_endpoint(0, 0, Some("http://a.example")).unwrap();
    set.set_interval(0, 120).unwrap();
    set.set_fallback(0, Some("http://f.example")).unwrap();
    let old = set.get(0).unwrap().clone();

    set.set_endpoint(0, 0, Some("http://b.example")).unwrap();
    let new = set.get(0).unwrap();
    assert_eq!(new.endpoint.as_str(), "http://b.example");
    assert_eq!(
        (new.interval_s, new.fallback.as_ref()),
        (120, old.fallback.as_ref())
    );
    assert!(new.generation > old.generation);
    assert_eq!(new.destinations, new.generation);

    let before = new.generation;
    set.set_endpoint(0, 1, Some("http://b.example")).unwrap(); // only the profile
    assert_eq!(set.get(0).unwrap().profile_index, 1);
    assert!(set.get(0).unwrap().generation > before);

    // The same profile and endpoint again: nothing changes.
    let same = set.clone();
    set.set_endpoint(0, 1, Some("http://b.example")).unwrap();
    assert_eq!(set, same);
}

#[test]
fn a_deleted_and_recreated_slot_inherits_nothing() {
    let mut set = set();
    set.set_endpoint(1, 0, Some("http://a.example")).unwrap();
    set.set_interval(1, 300).unwrap();
    set.set_fallback(1, Some("http://f.example")).unwrap();
    let old = set.get(1).unwrap().generation;
    set.set_endpoint(1, 0, None).unwrap();
    set.set_endpoint(1, 0, Some("http://a.example")).unwrap();
    let new = set.get(1).unwrap();
    assert!(new.generation > old);
    assert_eq!(new.interval_s, DEFAULT_INTERVAL_S);
    assert_eq!(new.fallback, None);
    assert!(set.validate().is_ok());
}

#[test]
fn the_fallback_is_single_replaceable_removable_and_distinct_from_the_primary() {
    let mut set = set();
    set.set_endpoint(0, 0, Some("http://a.example:80")).unwrap();
    assert_eq!(
        set.set_fallback(0, Some("http://A.example")),
        Err(RouteError::DuplicateEndpoint),
        "same host and default port"
    );
    set.set_fallback(0, Some("http://f1.example")).unwrap();
    let generation = set.get(0).unwrap().generation;
    let destinations = set.get(0).unwrap().destinations;
    set.set_fallback(0, Some("http://f2.example")).unwrap();
    let route = set.get(0).unwrap();
    assert_eq!(
        route.fallback.as_ref().unwrap().as_str(),
        "http://f2.example"
    );
    assert_eq!(
        route.generation, generation,
        "the route itself is unchanged"
    );
    assert!(route.destinations > destinations);

    let same = set.clone();
    set.set_fallback(0, Some("http://f2.example")).unwrap();
    assert_eq!(set, same);

    set.set_fallback(0, None).unwrap();
    assert_eq!(set.get(0).unwrap().fallback, None);
    let same = set.clone();
    set.set_fallback(0, None).unwrap();
    assert_eq!(set, same);

    // A new primary equal to the fallback is refused and changes nothing.
    set.set_fallback(0, Some("http://f1.example")).unwrap();
    let before = set.clone();
    assert_eq!(
        set.set_endpoint(0, 0, Some("http://f1.example")),
        Err(RouteError::DuplicateEndpoint)
    );
    assert_eq!(set, before);
}

#[test]
fn changing_one_slot_never_changes_another() {
    let mut set = set();
    set.set_endpoint(0, 0, Some("http://a.example")).unwrap();
    set.set_endpoint(1, 1, Some("http://b.example")).unwrap();
    let slot1 = set.get(1).unwrap().clone();
    set.set_endpoint(0, 1, Some("http://c.example")).unwrap();
    set.set_interval(0, 90).unwrap();
    set.set_fallback(0, Some("http://f.example")).unwrap();
    set.set_endpoint(0, 0, None).unwrap();
    assert_eq!(set.get(1), Some(&slot1));
}

#[test]
fn validate_rejects_inconsistent_stored_values() {
    let mut set = set();
    set.set_endpoint(0, 0, Some("http://a.example")).unwrap();
    set.set_fallback(0, Some("http://f.example")).unwrap();
    assert!(set.validate().is_ok());

    let mut bad = set.clone();
    bad.slots[0].as_mut().unwrap().interval_s = 1;
    assert_eq!(bad.validate(), Err(RouteError::InvalidInterval));

    let mut bad = set.clone();
    bad.slots[0].as_mut().unwrap().generation = 0;
    assert_eq!(bad.validate(), Err(RouteError::InconsistentGeneration));

    let mut bad = set.clone();
    bad.generation = 0;
    assert_eq!(bad.validate(), Err(RouteError::InconsistentGeneration));

    let mut bad = set.clone();
    bad.slots[0].as_mut().unwrap().fallback = Some(EndpointUri::new("http://a.example").unwrap());
    assert_eq!(bad.validate(), Err(RouteError::DuplicateEndpoint));
}

#[test]
fn the_section_round_trips_and_another_capacity_is_refused() {
    use rohi_config::ConfigSection;
    let mut set = Routes::<2>::default();
    set.set_endpoint(1, 1, Some("http://a.example")).unwrap();
    set.set_fallback(1, Some("http://f.example")).unwrap();
    let section = ConnectivitySection(set);
    let mut buffer = [0u8; 512];
    let mut writer = &mut buffer[..];
    minicbor::encode(&section, &mut writer).unwrap();
    let written = 512 - writer.len();
    let bytes = &buffer[..written];
    assert_eq!(written, minicbor::len(&section));
    let back: ConnectivitySection<2> = minicbor::decode(bytes).unwrap();
    assert_eq!(back, section);
    assert!(minicbor::decode::<ConnectivitySection<1>>(bytes).is_err());
    assert!(minicbor::decode::<ConnectivitySection<3>>(bytes).is_err());
    assert_eq!(ConnectivitySection::<2>::VERSION, 2);
}

#[test]
fn the_two_slot_device_section_fits_the_budget_with_the_longest_endpoints() {
    let long = |c: char| {
        let mut s = std::string::String::from("http://");
        while s.len() < MAX_ENDPOINT_LEN {
            s.push(c);
        }
        s
    };
    let mut set = Routes::<2>::default();
    for index in 0..2 {
        set.set_endpoint(index, 1, Some(&long('a'))).unwrap();
        set.set_fallback(index, Some(&long('b'))).unwrap();
    }
    let len = minicbor::len(ConnectivitySection(set));
    assert!(len <= rohi_config::MAX_SECTION_PAYLOAD, "{len}");
}
