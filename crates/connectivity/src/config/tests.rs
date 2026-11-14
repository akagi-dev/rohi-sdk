use super::*;

type Set = RouteSet;

fn binding(id: &str, incarnation: u32) -> ProfileBinding {
    ProfileBinding {
        id: PacketProfileId::new(id).unwrap(),
        incarnation,
    }
}

fn set() -> Set {
    Set::default()
}

#[test]
fn there_is_no_route_until_one_is_added_and_the_defaults_are_documented() {
    let mut set = set();
    assert!(set.routes.is_empty());
    set.add_route(
        "community",
        binding("environment", 3),
        "http://ingress.example.org",
    )
    .unwrap();
    let route = set.get("community").unwrap();
    assert_eq!(route.interval_s, 60);
    assert!(route.fallbacks.is_empty());
    assert_eq!(route.profile.incarnation, 3);
}

#[test]
fn ids_endpoints_and_capacity_are_validated() {
    let mut set = set();
    let profile = || binding("p", 1);
    for bad in ["", "Upper", "a b", "abcdefghijklmnopq"] {
        assert_eq!(
            set.add_route(bad, profile(), "http://a.example"),
            Err(RouteError::InvalidId)
        );
    }
    for bad in [
        "https://a.example",
        "http://a.example/x",
        "a.example",
        "http://",
    ] {
        assert_eq!(
            set.add_route("r", profile(), bad),
            Err(RouteError::InvalidEndpoint),
            "{bad}"
        );
    }
    set.add_route("a", profile(), "http://a.example").unwrap();
    assert_eq!(
        set.add_route("a", profile(), "http://b.example"),
        Err(RouteError::DuplicateRoute)
    );
    set.add_route("b", profile(), "http://a.example").unwrap(); // the same endpoint on another route is fine
    set.add_route("c", profile(), "http://c.example").unwrap();
    assert_eq!(
        set.add_route("d", profile(), "http://d.example"),
        Err(RouteError::TooManyRoutes)
    );
}

#[test]
fn only_the_interval_changes_in_place_within_its_range() {
    let mut set = set();
    set.add_route("a", binding("p", 1), "http://a.example")
        .unwrap();
    let before = set.get("a").unwrap().clone();
    set.set_interval("a", 120).unwrap();
    let after = set.get("a").unwrap();
    assert_eq!(after.interval_s, 120);
    assert_eq!(
        (
            &after.id,
            &after.profile,
            &after.primary,
            after.generation,
            after.destinations
        ),
        (
            &before.id,
            &before.profile,
            &before.primary,
            before.generation,
            before.destinations
        ),
        "an interval edit does not change the identity or the destinations"
    );
    for bad in [0, 9, 86_401] {
        assert_eq!(set.set_interval("a", bad), Err(RouteError::InvalidInterval));
    }
    set.set_interval("a", 10).unwrap();
    set.set_interval("a", 86_400).unwrap();
    assert_eq!(set.set_interval("x", 60), Err(RouteError::RouteNotFound));
}

#[test]
fn fallbacks_append_in_priority_order_and_reject_duplicates() {
    let mut set = set();
    set.add_route("a", binding("p", 1), "http://primary.example")
        .unwrap();
    set.add_fallback("a", "one", "http://one.example").unwrap();
    set.add_fallback("a", "two", "http://two.example:8080")
        .unwrap();
    let ids: std::vec::Vec<_> = set
        .get("a")
        .unwrap()
        .fallbacks
        .iter()
        .map(|f| f.id.as_str())
        .collect();
    assert_eq!(ids, ["one", "two"]);
    assert_eq!(
        set.add_fallback("a", "three", "http://three.example"),
        Err(RouteError::TooManyFallbacks)
    );
    set.remove_fallback("a", "one").unwrap();
    assert_eq!(
        set.remove_fallback("a", "one"),
        Err(RouteError::FallbackNotFound)
    );
    // Duplicates: the id, and the endpoint of the primary or of another
    // fallback (host compared without case).
    assert_eq!(
        set.add_fallback("a", "two", "http://x.example"),
        Err(RouteError::DuplicateFallback)
    );
    assert_eq!(
        set.add_fallback("a", "x", "http://PRIMARY.example"),
        Err(RouteError::DuplicateEndpoint)
    );
    assert_eq!(
        set.add_fallback("a", "x", "http://two.example:8080"),
        Err(RouteError::DuplicateEndpoint)
    );
    assert!(
        set.add_fallback("a", "x", "http://two.example:8081")
            .is_ok()
    );
    assert_eq!(
        set.add_fallback("nope", "x", "http://y.example"),
        Err(RouteError::RouteNotFound)
    );
}

#[test]
fn a_destination_edit_marks_the_route_and_a_recreated_route_is_a_new_generation() {
    let mut set = set();
    set.add_route("a", binding("p", 1), "http://a.example")
        .unwrap();
    let created = set.get("a").unwrap().clone();
    set.set_interval("a", 90).unwrap();
    assert_eq!(set.get("a").unwrap().destinations, created.destinations);
    set.add_fallback("a", "f", "http://f.example").unwrap();
    assert!(set.get("a").unwrap().destinations > created.destinations);

    set.remove_route("a").unwrap();
    set.add_route("a", binding("p", 1), "http://a.example")
        .unwrap();
    let again = set.get("a").unwrap();
    assert!(
        again.generation > created.generation,
        "no old queue or state is inherited"
    );
    assert!(again.fallbacks.is_empty());
    assert_eq!(again.interval_s, 60);
}

fn round_trip(set: &Set) -> Set {
    let mut buf = [0u8; 1024];
    let len = {
        let mut cursor = minicbor::encode::write::Cursor::new(&mut buf[..]);
        minicbor::encode(set, &mut cursor).unwrap();
        cursor.position()
    };
    assert_eq!(minicbor::len(set), len);
    minicbor::decode(&buf[..len]).unwrap()
}

#[test]
fn the_largest_configuration_fits_the_section_budget() {
    let mut set = set();
    let long = |n: usize| std::format!("http://{}.example.org:8080", "h".repeat(n));
    for id in ["aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb", "cccccccccccccccc"] {
        set.add_route(
            id,
            binding("pppppppppppppppppppppppp", u32::MAX),
            &std::format!("http://{}.example.org:8080", "p".repeat(32)),
        )
        .unwrap();
        for (n, f) in ["f1", "f2"].into_iter().enumerate() {
            let uri = long(30 + n);
            assert!(uri.len() <= MAX_ENDPOINT_LEN_FOR_TEST);
            set.add_fallback(id, f, &uri).unwrap();
        }
    }
    assert_eq!(round_trip(&set), set);
    assert!(
        minicbor::len(&set) <= rohi_config::MAX_SECTION_PAYLOAD,
        "{}",
        minicbor::len(&set)
    );
    assert!(set.validate().is_ok());
}

const MAX_ENDPOINT_LEN_FOR_TEST: usize = crate::endpoint::MAX_ENDPOINT_LEN;

#[test]
fn a_broken_stored_value_fails_validation() {
    let mut set = set();
    set.add_route("a", binding("p", 1), "http://a.example")
        .unwrap();
    set.add_fallback("a", "f", "http://f.example").unwrap();
    assert!(set.validate().is_ok());

    let mut bad = set.clone();
    bad.routes[0].interval_s = 5;
    assert_eq!(bad.validate(), Err(RouteError::InvalidInterval));
    let mut bad = set.clone();
    bad.routes[0].fallbacks[0].endpoint = bad.routes[0].primary.clone();
    assert_eq!(bad.validate(), Err(RouteError::DuplicateEndpoint));
    let mut bad = set.clone();
    bad.routes[0].generation = bad.generation + 1;
    assert!(bad.validate().is_err());
}
