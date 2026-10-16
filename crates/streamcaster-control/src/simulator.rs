use super::*;

#[async_trait]
pub trait SimulatedStreamCasterWriteApi: StreamCasterReadApi {
    async fn replace_effective_settings(
        &self,
        desired: StreamCasterEffectiveSettings,
    ) -> Result<(), StreamCasterError>;
    async fn restore_effective_settings(
        &self,
        snapshot: StreamCasterEffectiveSettings,
    ) -> Result<(), StreamCasterError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimulatorFailure {
    None,
    Apply,
    VerifyDrift,
    Rollback,
}

#[derive(Clone)]
pub struct SimulatedStreamCaster {
    capabilities: Arc<Mutex<StreamCasterCapabilities>>,
    effective: Arc<Mutex<StreamCasterEffectiveSettings>>,
    failure: Arc<Mutex<SimulatorFailure>>,
}

impl SimulatedStreamCaster {
    pub fn new(
        capabilities: StreamCasterCapabilities,
        effective: StreamCasterEffectiveSettings,
    ) -> Self {
        Self {
            capabilities: Arc::new(Mutex::new(capabilities)),
            effective: Arc::new(Mutex::new(effective)),
            failure: Arc::new(Mutex::new(SimulatorFailure::None)),
        }
    }

    pub async fn set_failure(&self, failure: SimulatorFailure) {
        *self.failure.lock().await = failure;
    }
}

#[async_trait]
impl StreamCasterReadApi for SimulatedStreamCaster {
    async fn read_capabilities(
        &self,
        observed_at_ms: u64,
    ) -> Result<StreamCasterCapabilities, StreamCasterError> {
        let mut value = self.capabilities.lock().await.clone();
        value.observed_at_ms = observed_at_ms;
        Ok(value)
    }

    async fn read_effective_settings(
        &self,
        observed_at_ms: u64,
    ) -> Result<StreamCasterEffectiveSettings, StreamCasterError> {
        let mut value = self.effective.lock().await.clone();
        value.observed_at_ms = observed_at_ms;
        Ok(value)
    }
}

#[async_trait]
impl SimulatedStreamCasterWriteApi for SimulatedStreamCaster {
    async fn replace_effective_settings(
        &self,
        mut desired: StreamCasterEffectiveSettings,
    ) -> Result<(), StreamCasterError> {
        match *self.failure.lock().await {
            SimulatorFailure::Apply => return Err(StreamCasterError::SimulatedApplyFailure),
            SimulatorFailure::VerifyDrift => desired.network_id.push_str("-DRIFT"),
            _ => {}
        }
        *self.effective.lock().await = desired;
        Ok(())
    }

    async fn restore_effective_settings(
        &self,
        snapshot: StreamCasterEffectiveSettings,
    ) -> Result<(), StreamCasterError> {
        if *self.failure.lock().await == SimulatorFailure::Rollback {
            return Err(StreamCasterError::SimulatedRollbackFailure);
        }
        *self.effective.lock().await = snapshot;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreparedStreamCasterTransaction {
    pub generation: u64,
    pub snapshot: StreamCasterEffectiveSettings,
    pub desired: StreamCasterEffectiveSettings,
    pub gates: StreamCasterActivationGates,
}

pub struct StreamCasterTransactionEngine<A> {
    api: A,
}

impl<A> StreamCasterTransactionEngine<A> {
    pub fn new(api: A) -> Self {
        Self { api }
    }
}

impl<A: StreamCasterReadApi> StreamCasterTransactionEngine<A> {
    pub async fn prepare(
        &self,
        plan: &ArcRadioConfiguration,
        assignment: &StreamCasterDeviceAssignment,
        mut gates: StreamCasterActivationGates,
        observed_at_ms: u64,
    ) -> Result<PreparedStreamCasterTransaction, StreamCasterError> {
        assignment.validate()?;
        plan.assess()?;
        let group = plan
            .fleet
            .groups
            .iter()
            .find(|group| group.group_id == assignment.group_id)
            .ok_or_else(|| StreamCasterError::InvalidPlan("assigned group is missing".into()))?;
        if group.model != assignment.expected_model {
            return Err(StreamCasterError::InvalidPlan(
                "assignment model differs from fleet group".into(),
            ));
        }
        let capabilities = self.api.read_capabilities(observed_at_ms).await?;
        gates.live_capability_match = capabilities
            .model
            .is_some_and(|model| models_compatible(assignment.expected_model, model))
            && capabilities
                .supported_frequency_profiles
                .iter()
                .any(|profile| profile.supports(plan, group.antenna_mask));
        gates.scheduled_activation_supported = capabilities.scheduled_activation_supported;
        gates.hardware_apply_enabled = assignment.hardware_apply_enabled;
        gates.antenna_installation_resolved &= assignment
            .antenna_installation_profile_id
            .as_ref()
            .is_some_and(|value| !value.is_empty());
        // Credential references are deliberately not forwarded by ARC. The
        // sidecar resolves its protected local mount and supplies this gate.
        let snapshot = self.api.read_effective_settings(observed_at_ms).await?;
        gates.rollback_snapshot_staged = true;
        let desired = StreamCasterEffectiveSettings {
            observed_at_ms,
            node_id: snapshot.node_id,
            system_name: snapshot.system_name.clone(),
            network_id: plan.network.network_id.clone(),
            center_frequency_mhz: plan.network.center_frequency_mhz,
            bandwidth_mhz: plan.network.bandwidth_mhz,
            link_distance_m: plan
                .network
                .link_distance_m
                .unwrap_or_else(|| (plan.network.maximum_node_distance_m * 1.15).ceil() as u32),
            antenna_mask: group.antenna_mask,
            max_power_enabled: Some(matches!(
                group.transmit_power,
                TransmitPowerMode::MaxSupported
            )),
            transmit_power_dbm_per_port: match group.transmit_power {
                TransmitPowerMode::MaxSupported => snapshot.transmit_power_dbm_per_port,
                TransmitPowerMode::TargetDbm { dbm } => Some(dbm),
            },
        };
        Ok(PreparedStreamCasterTransaction {
            generation: plan.generation,
            snapshot,
            desired,
            gates,
        })
    }
}

fn models_compatible(expected: StreamCasterModel, observed: StreamCasterModel) -> bool {
    expected == observed
        || (expected == StreamCasterModel::Sl5200LiteEstimated
            && matches!(
                observed,
                StreamCasterModel::Sl5200
                    | StreamCasterModel::Sl5205
                    | StreamCasterModel::Sl5210
                    | StreamCasterModel::Sl5220
            ))
}

impl<A: SimulatedStreamCasterWriteApi> StreamCasterTransactionEngine<A> {
    pub async fn apply_simulated(
        &self,
        prepared: PreparedStreamCasterTransaction,
        mechanism: FleetActivationMechanism,
        observed_at_ms: u64,
    ) -> Result<StreamCasterEffectiveSettings, StreamCasterError> {
        if !prepared.gates.ready_for_prepare() || !prepared.gates.supports(mechanism) {
            return Err(StreamCasterError::ActivationGatesFailed);
        }
        if let Err(error) = self
            .api
            .replace_effective_settings(prepared.desired.clone())
            .await
        {
            self.api
                .restore_effective_settings(prepared.snapshot)
                .await
                .map_err(|rollback| StreamCasterError::ApplyAndRollback {
                    apply: error.to_string(),
                    rollback: rollback.to_string(),
                })?;
            return Err(error);
        }
        let effective = self.api.read_effective_settings(observed_at_ms).await?;
        if !settings_match(&effective, &prepared.desired) {
            self.api
                .restore_effective_settings(prepared.snapshot)
                .await
                .map_err(|rollback| StreamCasterError::ApplyAndRollback {
                    apply: "verification drift".into(),
                    rollback: rollback.to_string(),
                })?;
            return Err(StreamCasterError::VerificationDrift {
                desired: serde_json::to_value(prepared.desired).unwrap_or(Value::Null),
                effective: serde_json::to_value(effective).unwrap_or(Value::Null),
            });
        }
        Ok(effective)
    }
}

fn settings_match(
    effective: &StreamCasterEffectiveSettings,
    desired: &StreamCasterEffectiveSettings,
) -> bool {
    effective.network_id == desired.network_id
        && (effective.center_frequency_mhz - desired.center_frequency_mhz).abs() < 0.001
        && effective.bandwidth_mhz == desired.bandwidth_mhz
        && effective.link_distance_m == desired.link_distance_m
        && effective.antenna_mask == desired.antenna_mask
        && effective.max_power_enabled == desired.max_power_enabled
        && (desired.max_power_enabled == Some(true)
            || effective.transmit_power_dbm_per_port == desired.transmit_power_dbm_per_port)
}
