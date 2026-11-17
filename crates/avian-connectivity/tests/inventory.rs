use avian_connectivity::{
    ConnectionInventory, DiscoveryCache, Endpoint, Metering, Underlay, inventory::*,
};
use std::net::IpAddr;

#[test]
fn platform_inventory_preserves_real_addresses_and_carrier_state() {
    let linux=parse_linux(r#"[{"ifname":"radio0","flags":["UP","LOWER_UP"],"addr_info":[{"local":"192.168.10.4"}]},{"ifname":"eth0","flags":["UP"],"addr_info":[]}]"#).unwrap();
    assert!(linux[0].available);
    assert!(!linux[1].available);
    let mac=parse_macos("en0: flags=8863<UP,BROADCAST,RUNNING,MULTICAST> mtu 1500\n\tinet 192.168.10.4 netmask 0xffffff00 broadcast 192.168.10.255\n\tinet6 fe80::1%en0 prefixlen 64\n\tstatus: active\nen1: flags=8823<UP,BROADCAST,MULTICAST> mtu 1500\n\tstatus: inactive\n").unwrap();
    assert_eq!(
        mac[0].addresses,
        vec![
            "192.168.10.4".parse::<IpAddr>().unwrap(),
            "fe80::1".parse::<IpAddr>().unwrap()
        ]
    );
    assert!(!mac[1].available);
    let windows=parse_windows(r#"[{"InterfaceAlias":"Wi-Fi","IPAddress":"192.168.10.4","AddressState":4},{"InterfaceAlias":"Wi-Fi","IPAddress":"2001:db8::1","AddressState":"Preferred"},{"InterfaceAlias":"Ethernet","IPAddress":"192.168.10.5","AddressState":2}]"#).unwrap();
    assert_eq!(
        windows
            .iter()
            .find(|row| row.interface_name == "Wi-Fi")
            .unwrap()
            .addresses
            .len(),
        2
    );
    assert!(
        !windows
            .iter()
            .find(|row| row.interface_name == "Ethernet")
            .unwrap()
            .available
    );
    assert!(windows.iter().all(|row| row.metering == Metering::Unknown));
}

#[test]
fn inventory_update_is_atomic_and_excludes_its_own_tunnel() {
    let radio = Underlay {
        interface_name: "radio0".into(),
        addresses: vec!["192.168.10.4".parse().unwrap()],
        available: true,
        metering: Metering::Unmetered,
    };
    let mut inventory = ConnectionInventory::default();
    inventory.update(vec![radio.clone()], "avian0").unwrap();
    assert!(
        inventory
            .update(vec![radio.clone(), radio.clone()], "avian0")
            .is_err()
    );
    assert_eq!(inventory.interfaces().count(), 1);
    let mut owned = radio;
    owned.interface_name = "avian0".into();
    let delta = inventory.update(vec![owned], "avian0").unwrap();
    assert_eq!(delta.removed, vec!["radio0"]);
    assert_eq!(inventory.interfaces().count(), 0);
}

#[test]
fn untrusted_discovery_cannot_inject_third_party_endpoints_or_partial_updates() {
    let mut cache = DiscoveryCache::new("field".into(), 8).unwrap();
    let source = "192.168.10.4".parse().unwrap();
    let radio = Underlay {
        interface_name: "radio0".into(),
        addresses: vec!["192.168.10.1".parse().unwrap()],
        available: true,
        metering: Metering::Unknown,
    };
    assert!(cache.accept(br#"{"version":1,"network_id":"field","endpoints":["tcp://192.168.10.4:11010","tcp://127.0.0.1:9101"]}"#,source,&radio,0).is_err());
    assert!(cache.endpoints(0).is_empty());
    cache
        .accept(
            br#"{"version":1,"network_id":"field","endpoints":["tcp://192.168.10.4:11010"]}"#,
            source,
            &radio,
            0,
        )
        .unwrap();
    assert_eq!(cache.endpoints(29_999).len(), 1);
    assert!(cache.endpoints(30_000).is_empty());
    for endpoint in [
        "tcp://user:secret@host:1",
        "http://host:1",
        "udp://0.0.0.0:11010",
        "tcp://host:11010?flag=1",
        "tcp://host:11010\n",
    ] {
        assert!(Endpoint::parse(endpoint).is_err(), "{endpoint}");
    }
    assert!(serde_json::from_str::<Endpoint>(r#""tcp://host:11010?flag=1""#).is_err());
}
