use mesh_core::{
    ArcRadioConfiguration, StreamCasterDeviceAssignment, StreamCasterOperationRequest,
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
fn v1_cross_repo_fixtures_match_the_authoritative_rust_contracts() {
    assert_semantic_round_trip::<ArcRadioConfiguration>(FLEET_PLAN);
    assert_semantic_round_trip::<StreamCasterDeviceAssignment>(DEVICE_ASSIGNMENT);
    assert_semantic_round_trip::<StreamCasterOperationRequest>(OPERATION_REQUEST);
    assert_semantic_round_trip::<StreamCasterOperationStatus>(OPERATION_STATUS);
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
