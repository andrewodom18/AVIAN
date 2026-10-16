use super::*;

pub(super) async fn process_operation(
    state: &Arc<RwLock<ServiceState>>,
    request: StreamCasterOperationRequest,
    now_ms: u64,
) -> StreamCasterOperationStatus {
    if matches!(
        request.intent,
        StreamCasterOperationIntent::Activate
            | StreamCasterOperationIntent::Confirm
            | StreamCasterOperationIntent::Rollback
    ) {
        return blocked_status(
            &request,
            now_ms,
            StreamCasterActivationGates::default(),
            "external_management_required",
            "Silvus hardware configuration is owned by the external radio-management API; AVIAN validates and distributes planning state only.",
        );
    }

    if let Err(error) = request.validate() {
        return failed_status(&request, now_ms, "invalid_operation", error.to_string());
    }

    let configured_management = state.read().await.management_address.clone();
    if let Some(configured_management) = configured_management {
        let requested_management = management_host(&request.assignment.management_address)
            .unwrap_or_else(|| request.assignment.management_address.clone());
        if configured_management != requested_management {
            return blocked_status(
                &request,
                now_ms,
                StreamCasterActivationGates::default(),
                "management_binding_mismatch",
                "The sidecar radio endpoint does not match the canonical ARC management address.",
            );
        }
    }

    persist_plan(state, &request.fleet_plan, now_ms).await;
    let evidence = evidence_gates(state, &request).await;
    {
        let mut state = state.write().await;
        if state.simulate_radio && matches!(state.backend, Backend::ContractOnly) {
            match simulator_for(&request.fleet_plan, &request.assignment) {
                Ok(simulator) => state.backend = Backend::Simulator(simulator),
                Err(error) => {
                    return blocked_status(
                        &request,
                        now_ms,
                        evidence,
                        "simulator_initialization_failed",
                        &error.to_string(),
                    );
                }
            }
        }
    }
    let backend_kind = {
        let state = state.read().await;
        match &state.backend {
            Backend::ContractOnly => 0,
            Backend::Simulator(_) => 2,
        }
    };

    if backend_kind == 0 {
        return blocked_status(
            &request,
            now_ms,
            evidence,
            "radio_backend_unavailable",
            "Contract validation succeeded; no radio URL or simulator is configured.",
        );
    }

    let prepared = {
        let state_guard = state.read().await;
        match &state_guard.backend {
            Backend::Simulator(simulator) => {
                StreamCasterTransactionEngine::new(simulator.clone())
                    .prepare(&request.fleet_plan, &request.assignment, evidence, now_ms)
                    .await
            }
            Backend::ContractOnly => unreachable!(),
        }
    };
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            return blocked_status(
                &request,
                now_ms,
                evidence,
                "radio_inspection_failed",
                &error.to_string(),
            );
        }
    };
    let capabilities = read_capabilities(state, now_ms).await.ok();
    let effective = Some(prepared.snapshot.clone());
    let plugin_ready = prepared.gates.live_capability_match
        && prepared.gates.regulatory_authorized
        && prepared.gates.antenna_installation_resolved
        && prepared.gates.credential_resolved
        && prepared.gates.rollback_snapshot_staged;
    StreamCasterOperationStatus {
        schema_version: STREAMCASTER_CONTROL_SCHEMA_VERSION,
        request_id: request.request_id.clone(),
        node_id: request.assignment.node_id.clone(),
        generation: request.fleet_plan.generation,
        observed_at_ms: now_ms,
        phase: if plugin_ready {
            StreamCasterApplyPhase::Prepared
        } else {
            StreamCasterApplyPhase::Blocked
        },
        gates: prepared.gates,
        capabilities,
        effective,
        error_code: (!plugin_ready).then(|| "activation_evidence_incomplete".into()),
        message: (!plugin_ready).then(|| {
            "Read-only inspection succeeded; regulatory, antenna, credential, or capability evidence is incomplete.".into()
        }),
    }
}

pub(super) async fn read_capabilities(
    state: &Arc<RwLock<ServiceState>>,
    now_ms: u64,
) -> Result<StreamCasterCapabilities, streamcaster_control::StreamCasterError> {
    let state = state.read().await;
    match &state.backend {
        Backend::Simulator(simulator) => simulator.read_capabilities(now_ms).await,
        Backend::ContractOnly => unreachable!(),
    }
}

pub(super) fn blocked_status(
    request: &StreamCasterOperationRequest,
    now_ms: u64,
    gates: StreamCasterActivationGates,
    code: &str,
    message: &str,
) -> StreamCasterOperationStatus {
    StreamCasterOperationStatus {
        schema_version: STREAMCASTER_CONTROL_SCHEMA_VERSION,
        request_id: request.request_id.clone(),
        node_id: request.assignment.node_id.clone(),
        generation: request.fleet_plan.generation,
        observed_at_ms: now_ms,
        phase: StreamCasterApplyPhase::Blocked,
        gates,
        capabilities: None,
        effective: None,
        error_code: Some(code.into()),
        message: Some(message.into()),
    }
}

pub(super) fn failed_status(
    request: &StreamCasterOperationRequest,
    now_ms: u64,
    code: &str,
    message: String,
) -> StreamCasterOperationStatus {
    let mut status = blocked_status(
        request,
        now_ms,
        StreamCasterActivationGates::default(),
        code,
        &message,
    );
    status.phase = StreamCasterApplyPhase::Failed;
    status
}

pub(super) fn invalid_status(message: String, now_ms: u64) -> StreamCasterOperationStatus {
    StreamCasterOperationStatus {
        schema_version: STREAMCASTER_CONTROL_SCHEMA_VERSION,
        request_id: "invalid".into(),
        node_id: NodeId::from("unknown"),
        generation: 0,
        observed_at_ms: now_ms,
        phase: StreamCasterApplyPhase::Failed,
        gates: StreamCasterActivationGates::default(),
        capabilities: None,
        effective: None,
        error_code: Some("invalid_operation_json".into()),
        message: Some(message),
    }
}

pub(super) fn simulator_for(
    plan: &ArcRadioConfiguration,
    assignment: &StreamCasterDeviceAssignment,
) -> anyhow::Result<SimulatedStreamCaster> {
    let group = plan
        .fleet
        .groups
        .iter()
        .find(|group| group.group_id == assignment.group_id)
        .context("simulator assignment group")?;
    let capabilities = StreamCasterCapabilities {
        observed_at_ms: now_unix_ms(),
        model: Some(assignment.expected_model),
        firmware_version: Some("simulated-v1".into()),
        supported_frequency_profiles: vec![StreamCasterFrequencyProfile {
            center_frequency_mhz: plan.network.center_frequency_mhz,
            bandwidth_mhz: plan.network.bandwidth_mhz,
            antenna_mask: group.antenna_mask,
        }],
        scheduled_activation_supported: true,
        dual_profile_supported: true,
    };
    let effective = StreamCasterEffectiveSettings {
        observed_at_ms: now_unix_ms(),
        node_id: None,
        system_name: Some(assignment.node_id.to_string()),
        network_id: "SIMULATOR-INITIAL".into(),
        center_frequency_mhz: plan.network.center_frequency_mhz,
        bandwidth_mhz: plan.network.bandwidth_mhz,
        link_distance_m: plan.network.link_distance_m.unwrap_or(1),
        antenna_mask: group.antenna_mask,
        max_power_enabled: Some(matches!(
            group.transmit_power,
            TransmitPowerMode::MaxSupported
        )),
        transmit_power_dbm_per_port: match group.transmit_power {
            TransmitPowerMode::MaxSupported => None,
            TransmitPowerMode::TargetDbm { dbm } => Some(dbm),
        },
    };
    Ok(SimulatedStreamCaster::new(capabilities, effective))
}
