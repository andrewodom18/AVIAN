use mesh_core::{
    ArcRadioConfiguration, RadioDiscoveryIntakeEnvelope, RadioDiscoveryObservation,
    RadioObservationAuthority, StreamCasterDeviceAssignment, StreamCasterOperationRequest,
    StreamCasterOperationStatus,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

const FLEET_PLAN: &str =
    include_str!("../../../apps/arc-radio-plugin/tests/fixtures/fleet-plan.v1.json");
const DEVICE_ASSIGNMENT: &str =
    include_str!("../../../apps/arc-radio-plugin/tests/fixtures/device-assignment.v1.json");
const OPERATION_REQUEST: &str =
    include_str!("../../../apps/arc-radio-plugin/tests/fixtures/operation-request.v1.json");
const OPERATION_STATUS: &str =
    include_str!("../../../apps/arc-radio-plugin/tests/fixtures/operation-status.v1.json");
const RADIO_DISCOVERY: &str =
    include_str!("../../../apps/arc-radio-plugin/tests/fixtures/radio-discovery.v1.json");
const RADIO_DISCOVERY_V2: &str =
    include_str!("../../../apps/arc-radio-plugin/tests/fixtures/radio-discovery.v2.json");
const RADIO_DISCOVERY_INTAKE: &str =
    include_str!("../../../apps/arc-radio-plugin/tests/fixtures/radio-discovery-intake.v1.json");

fn assert_semantic_round_trip<T>(encoded: &str)
where
    T: DeserializeOwned + Serialize,
{
    let original: Value = serde_json::from_str(encoded).unwrap();
    let typed: T = serde_json::from_value(original.clone()).unwrap();
    let round_trip = serde_json::to_value(typed).unwrap();
    assert_eq!(round_trip, original);
}

#[test]
fn v2_radio_discovery_fixture_is_valid_and_configuration_authoritative() {
    let discovery: RadioDiscoveryObservation = serde_json::from_str(RADIO_DISCOVERY_V2).unwrap();
    discovery.validate().unwrap();
    assert!(discovery.is_authoritative_for_configuration_at(15_000));
    assert!(!discovery.is_authoritative_for_configuration_at(20_001));
    assert_semantic_round_trip::<RadioDiscoveryObservation>(RADIO_DISCOVERY_V2);
}

#[test]
fn avian_discovery_intake_is_fresh_diagnostic_candidate_not_chud_authority() {
    let intake: RadioDiscoveryIntakeEnvelope =
        serde_json::from_str(RADIO_DISCOVERY_INTAKE).unwrap();
    intake.validate_at(12_000).unwrap();
    assert_eq!(
        intake.observation.source_authority,
        Some(RadioObservationAuthority::AvianDiagnostic)
    );
    assert!(!intake
        .observation
        .is_authoritative_for_configuration_at(12_000));
    assert_semantic_round_trip::<RadioDiscoveryIntakeEnvelope>(RADIO_DISCOVERY_INTAKE);

    let mut authoritative = intake.clone();
    authoritative.observation.source_authority = Some(RadioObservationAuthority::ChudAuthoritative);
    assert!(authoritative.validate_at(12_000).is_err());
    let mut simulated = intake.clone();
    simulated.observation.source_authority = Some(RadioObservationAuthority::Simulation);
    assert!(simulated.validate_at(12_000).is_err());
    let mut stale = intake;
    stale.observation.expires_at_ms = Some(11_500);
    assert!(stale.validate_at(12_000).is_err());
}

#[test]
fn v1_cross_repo_fixtures_match_the_authoritative_rust_contracts() {
    assert_semantic_round_trip::<ArcRadioConfiguration>(FLEET_PLAN);
    assert_semantic_round_trip::<StreamCasterDeviceAssignment>(DEVICE_ASSIGNMENT);
    assert_semantic_round_trip::<StreamCasterOperationRequest>(OPERATION_REQUEST);
    assert_semantic_round_trip::<StreamCasterOperationStatus>(OPERATION_STATUS);
    assert_semantic_round_trip::<RadioDiscoveryObservation>(RADIO_DISCOVERY);
}

#[test]
fn operation_request_validates_and_contains_no_secret_values() {
    let request: StreamCasterOperationRequest = serde_json::from_str(OPERATION_REQUEST).unwrap();
    request.validate().unwrap();

    let lower = OPERATION_REQUEST.to_ascii_lowercase();
    for forbidden in ["password", "session_cookie", "private_key", "hmac_key"] {
        assert!(!lower.contains(forbidden), "fixture contains {forbidden}");
    }
}
