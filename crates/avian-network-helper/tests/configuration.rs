use avian_network_helper::{HelperConfiguration, Request, RequestPacket};
fn configuration() -> HelperConfiguration {
    HelperConfiguration {
        allowed_uid: 1000,
        runtime_directory: "/run/avian/test".into(),
        node_id: "12".repeat(32),
        tun_name: "aviantest0".into(),
        overlay_address: "10.64.0.2".parse().unwrap(),
        overlay_network: "10.64.0.0/24".parse().unwrap(),
        mtu: 1380,
        listen_port: 11010,
        allowed_underlay_interfaces: vec!["eth0".into()],
    }
}
#[test]
fn the_entire_overlay_must_stay_in_a_private_nondefault_network() {
    configuration().validate().unwrap();
    for (address, network) in [
        ("192.168.1.2", "192.0.0.0/8"),
        ("172.16.1.2", "172.0.0.0/8"),
        ("10.1.1.2", "10.0.0.0/7"),
        ("10.1.1.2", "0.0.0.0/0"),
        ("10.64.0.2", "10.64.0.1/24"),
    ] {
        let mut invalid = configuration();
        invalid.overlay_address = address.parse().unwrap();
        invalid.overlay_network = network.parse().unwrap();
        assert!(
            invalid.validate().is_err(),
            "accepted unsafe overlay {network}"
        );
    }
}
#[test]
fn root_application_identity_and_unapproved_interface_names_are_rejected() {
    let mut invalid = configuration();
    invalid.allowed_uid = 0;
    assert!(invalid.validate().is_err());
    for name in ["lo", "aviantest0", "../eth0", "eth0;command", ""] {
        let mut invalid = configuration();
        invalid.allowed_underlay_interfaces = vec![name.into()];
        assert!(invalid.validate().is_err());
    }
    let mut invalid = configuration();
    invalid.allowed_underlay_interfaces.push("eth0".into());
    assert!(invalid.validate().is_err());
}
#[test]
fn client_messages_cannot_supply_commands_paths_routes_or_raw_socket_types() {
    for value in [
        r#"{"version":1,"request":{"action":"prepare_network","command":"anything"}}"#,
        r#"{"version":1,"request":{"action":"release_network","path":"/etc"}}"#,
        r#"{"version":1,"request":{"action":"add_route","destination":"0.0.0.0/0"}}"#,
        r#"{"version":1,"request":{"action":"open_socket","transport":"raw","interface_name":"eth0","source_address":"192.0.2.1","destination":"192.0.2.2:11010"}}"#,
    ] {
        assert!(serde_json::from_str::<RequestPacket>(value).is_err());
    }
    let valid: RequestPacket =
        serde_json::from_str(r#"{"version":1,"request":{"action":"status"}}"#).unwrap();
    assert!(matches!(valid.request, Request::Status {}));
}
