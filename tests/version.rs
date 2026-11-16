use super::valid_version;

#[test]
fn release_versions_accept_semantic_prereleases() {
    for version in ["0.1.0", "0.1.0-rc.1", "1.2.3-nightly.20261116"] {
        assert!(valid_version(version), "rejected {version}");
    }
}

#[test]
fn malformed_versions_cannot_be_baked_into_release_binaries() {
    for version in [
        "0.1",
        "01.0.0",
        "0.1.0-rc.01",
        "0.1.0-",
        "0.1.0+build",
        "0.1.0\n",
    ] {
        assert!(!valid_version(version), "accepted {version:?}");
    }
}
