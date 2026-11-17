use avian_connectivity::{
    BootstrapConfiguration, DiscoveryMessage, Endpoint, Metering, Underlay,
    bootstrap::BootstrapCoordinator,
};
use std::net::IpAddr;

fn underlay(name: &str, address: &str) -> Underlay {
    Underlay {
        interface_name: name.into(),
        addresses: vec![address.parse().unwrap()],
        available: true,
        metering: Metering::Unmetered,
    }
}
fn coordinator(endpoints: &[&str], maximum: usize) -> BootstrapCoordinator {
    BootstrapCoordinator::new(
        BootstrapConfiguration {
            owned_endpoints: endpoints
                .iter()
                .map(|value| Endpoint::parse(value).unwrap())
                .collect(),
            ..Default::default()
        },
        "test-network".into(),
        64,
        maximum,
    )
    .unwrap()
}

#[test]
fn address_changes_remove_previous_bindings_before_reconnection() {
    let mut bootstrap = coordinator(&["tcp://192.0.2.4:11010", "udp://192.0.2.4:11010"], 16);
    let initial = bootstrap
        .reconcile(&[underlay("radio", "192.0.2.2")], &[], 0)
        .unwrap();
    assert_eq!(initial.added.len(), 2);
    assert!(initial.added.iter().all(|binding| {
        binding
            .engine_url
            .contains("?avian_bind=192.0.2.2&avian_interface=radio")
    }));
    assert!(
        bootstrap
            .reconcile(&[underlay("radio", "192.0.2.2")], &[], 1)
            .unwrap()
            .added
            .is_empty()
    );
    let change = bootstrap
        .reconcile(&[underlay("radio", "192.0.2.3")], &[], 2)
        .unwrap();
    assert_eq!(change.removed, initial.added);
    assert_eq!(change.added.len(), 2);
    assert!(
        change
            .added
            .iter()
            .all(|binding| binding.source_address == "192.0.2.3".parse::<IpAddr>().unwrap())
    );
    let mut disconnected = underlay("radio", "192.0.2.3");
    disconnected.available = false;
    let loss = bootstrap.reconcile(&[disconnected], &[], 3).unwrap();
    assert_eq!(loss.removed, change.added);
    assert!(bootstrap.desired().next().is_none());
}

#[test]
fn dns_seeds_are_family_bound_and_overlay_or_ambiguous_sources_are_excluded() {
    let mut bootstrap = coordinator(
        &["tcp://relay.example:11010", "udp://[2001:db8::4]:11010"],
        16,
    );
    let mut radio = underlay("radio", "192.0.2.2");
    radio.addresses.extend([
        "2001:db8::2".parse::<IpAddr>().unwrap(),
        "fe80::2".parse::<IpAddr>().unwrap(),
        "10.64.0.2".parse::<IpAddr>().unwrap(),
    ]);
    let delta = bootstrap
        .reconcile(
            &[radio.clone()],
            &["10.64.0.2".parse::<IpAddr>().unwrap()],
            0,
        )
        .unwrap();
    assert_eq!(delta.added.len(), 3);
    assert!(delta.added.iter().all(|binding| matches!(
        binding.source_address.to_string().as_str(),
        "192.0.2.2" | "2001:db8::2"
    )));
    let duplicate = underlay("other-radio", "192.0.2.2");
    let delta = bootstrap
        .reconcile(
            &[radio, duplicate],
            &["10.64.0.2".parse::<IpAddr>().unwrap()],
            1,
        )
        .unwrap();
    assert_eq!(delta.removed.len(), 1);
    assert!(
        bootstrap
            .desired()
            .all(|binding| binding.source_address.is_ipv6())
    );
}

#[test]
fn expired_untrusted_hints_are_removed_without_removing_owned_seeds() {
    let mut bootstrap = coordinator(&["tcp://192.0.2.4:11010"], 16);
    let radio = underlay("radio", "192.0.2.2");
    let hint = DiscoveryMessage {
        version: 1,
        network_id: "test-network".into(),
        endpoints: vec![Endpoint::parse("udp://192.0.2.5:11010").unwrap()],
    };
    bootstrap
        .accept_hint(
            &serde_json::to_vec(&hint).unwrap(),
            "192.0.2.5".parse().unwrap(),
            &radio,
            0,
        )
        .unwrap();
    assert_eq!(
        bootstrap
            .reconcile(std::slice::from_ref(&radio), &[], 0)
            .unwrap()
            .added
            .len(),
        2
    );
    let expired = bootstrap.reconcile(&[radio], &[], 30_001).unwrap();
    assert_eq!(expired.removed.len(), 1);
    assert!(expired.removed[0].endpoint.as_str().starts_with("udp://"));
    assert_eq!(bootstrap.desired().count(), 1);
}

#[test]
fn resource_exhaustion_does_not_partially_publish_a_new_connector_set() {
    let mut bootstrap = coordinator(&["tcp://192.0.2.4:11010"], 1);
    let radio = underlay("radio", "192.0.2.2");
    bootstrap
        .reconcile(std::slice::from_ref(&radio), &[], 0)
        .unwrap();
    assert!(
        bootstrap
            .reconcile(&[radio, underlay("wifi", "198.51.100.2")], &[], 1)
            .is_err()
    );
    assert_eq!(bootstrap.desired().count(), 1);
    assert_eq!(bootstrap.events_since(0).events.len(), 1);
}

#[test]
fn metering_changes_are_observable_and_lagging_event_readers_resynchronize() {
    let mut bootstrap = coordinator(&["tcp://192.0.2.4:11010"], 1);
    for index in 0..260 {
        let mut radio = underlay("radio", "192.0.2.2");
        radio.metering = if index % 2 == 0 {
            Metering::Unmetered
        } else {
            Metering::Unknown
        };
        bootstrap.reconcile(&[radio], &[], index).unwrap();
    }
    let events = bootstrap.events_since(0);
    assert!(events.resync_required);
    assert!(events.events.is_empty());
    assert_eq!(events.next_cursor, 260);
    assert!(bootstrap.events_since(259).events[0].added[0].restrict_best_effort);
}

#[test]
fn discovery_cannot_displace_owned_seeds_when_the_connection_budget_is_full() {
    let mut bootstrap = coordinator(&["tcp://192.0.2.4:11010"], 1);
    let radio = underlay("radio", "192.0.2.2");
    let hint = DiscoveryMessage {
        version: 1,
        network_id: "test-network".into(),
        endpoints: vec![Endpoint::parse("udp://192.0.2.5:11010").unwrap()],
    };
    bootstrap
        .accept_hint(
            &serde_json::to_vec(&hint).unwrap(),
            "192.0.2.5".parse().unwrap(),
            &radio,
            0,
        )
        .unwrap();
    let delta = bootstrap.reconcile(&[radio], &[], 0).unwrap();
    assert_eq!(delta.added.len(), 1);
    assert_eq!(delta.added[0].endpoint.as_str(), "tcp://192.0.2.4:11010");
}
