use super::*;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegulatoryEvidence {
    authorized: bool,
    allowed_profiles: Vec<RegulatoryProfile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegulatoryProfile {
    center_frequency_mhz: f64,
    bandwidth_mhz: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AntennaEvidence {
    profile_id: String,
    approved: bool,
    antenna_gain_dbi: f64,
    installation_loss_db: f64,
    calibrated_at_ms: u64,
}

pub(super) async fn evidence_gates(
    state: &Arc<RwLock<ServiceState>>,
    request: &StreamCasterOperationRequest,
) -> StreamCasterActivationGates {
    let state = state.read().await;
    StreamCasterActivationGates {
        regulatory_authorized: state
            .regulatory_evidence_file
            .as_deref()
            .is_some_and(|path| regulatory_evidence_matches(path, &request.fleet_plan)),
        antenna_installation_resolved: state
            .installation_evidence_dir
            .as_deref()
            .zip(
                request
                    .assignment
                    .antenna_installation_profile_id
                    .as_deref(),
            )
            .is_some_and(|(directory, profile)| antenna_evidence_matches(directory, profile)),
        // AVIAN does not resolve radio credentials. The simulator marks this
        // gate so it can validate the remaining planning inputs in isolation.
        credential_resolved: state.simulate_radio,
        ..Default::default()
    }
}

pub(super) fn regulatory_evidence_matches(path: &Path, plan: &ArcRadioConfiguration) -> bool {
    let Ok(encoded) = std::fs::read(path) else {
        return false;
    };
    let Ok(evidence) = serde_json::from_slice::<RegulatoryEvidence>(&encoded) else {
        return false;
    };
    evidence.authorized
        && evidence.allowed_profiles.iter().any(|profile| {
            profile.center_frequency_mhz.is_finite()
                && (profile.center_frequency_mhz - plan.network.center_frequency_mhz).abs() < 0.001
                && (profile.bandwidth_mhz - plan.network.bandwidth_mhz.as_mhz()).abs() < 0.001
        })
}

pub(super) fn antenna_evidence_matches(directory: &Path, profile_id: &str) -> bool {
    if profile_id.contains("..") || profile_id.starts_with('/') || profile_id.contains('\\') {
        return false;
    }
    let path = directory.join(format!("{profile_id}.json"));
    let Ok(encoded) = std::fs::read(path) else {
        return false;
    };
    let Ok(evidence) = serde_json::from_slice::<AntennaEvidence>(&encoded) else {
        return false;
    };
    evidence.profile_id == profile_id
        && evidence.approved
        && evidence.antenna_gain_dbi.is_finite()
        && evidence.installation_loss_db.is_finite()
        && evidence.installation_loss_db >= 0.0
        && evidence.calibrated_at_ms > 0
}
