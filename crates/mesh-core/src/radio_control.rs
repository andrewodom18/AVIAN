use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{ArcRadioConfiguration, ChannelBandwidthMhz, NodeId, StreamCasterModel};

pub const STREAMCASTER_CONTROL_SCHEMA_VERSION: u16 = 1;

/// The small, per-device portion of StreamCaster desired configuration.
/// Fleet topology and shared RF intent intentionally live in
/// `ArcRadioConfiguration`, which is authored once and distributed by PEAT.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamCasterDeviceAssignment {
    pub schema_version: u16,
    pub node_id: NodeId,
    pub group_id: String,
    pub expected_model: StreamCasterModel,
    pub management_interface: String,
    /// Linux interface carrying operational mesh traffic. This is separate
    /// from the isolated management plane used for vendor API calls.
    pub data_interface: String,
    pub management_address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub antenna_installation_profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_reference: Option<String>,
    #[serde(default)]
    pub hardware_apply_enabled: bool,
}

impl StreamCasterDeviceAssignment {
    pub fn validate(&self) -> Result<(), StreamCasterControlError> {
        if self.schema_version != STREAMCASTER_CONTROL_SCHEMA_VERSION {
            return Err(StreamCasterControlError::UnsupportedSchemaVersion(
                self.schema_version,
            ));
        }
        if self.node_id.as_str().trim().is_empty() {
            return Err(StreamCasterControlError::EmptyNodeId);
        }
        validate_identifier("group_id", &self.group_id)?;
        validate_interface(&self.management_interface)?;
        validate_interface(&self.data_interface)?;
        self.management_address.parse::<IpAddr>().map_err(|_| {
            StreamCasterControlError::InvalidManagementAddress(self.management_address.clone())
        })?;
        validate_optional_reference(
            "antenna_installation_profile_id",
            self.antenna_installation_profile_id.as_deref(),
        )?;
        validate_optional_reference("credential_reference", self.credential_reference.as_deref())?;
        Ok(())
    }
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), StreamCasterControlError> {
    let valid = !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(StreamCasterControlError::InvalidIdentifier {
            field,
            value: value.to_owned(),
        })
    }
}

fn validate_interface(value: &str) -> Result<(), StreamCasterControlError> {
    let valid = !value.is_empty()
        && value.len() <= 15
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'));
    if valid {
        Ok(())
    } else {
        Err(StreamCasterControlError::InvalidManagementInterface(
            value.to_owned(),
        ))
    }
}

fn validate_optional_reference(
    field: &'static str,
    value: Option<&str>,
) -> Result<(), StreamCasterControlError> {
    if let Some(value) = value {
        if value.is_empty()
            || value.len() > 255
            || value.contains("..")
            || value.starts_with('/')
            || value.contains('\\')
        {
            return Err(StreamCasterControlError::InvalidReference {
                field,
                value: value.to_owned(),
            });
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamCasterFrequencyProfile {
    pub center_frequency_mhz: f64,
    pub bandwidth_mhz: ChannelBandwidthMhz,
    pub antenna_mask: u8,
}

impl StreamCasterFrequencyProfile {
    pub fn supports(&self, desired: &ArcRadioConfiguration, local_antenna_mask: u8) -> bool {
        (self.center_frequency_mhz - desired.network.center_frequency_mhz).abs() < 0.001
            && self.bandwidth_mhz == desired.network.bandwidth_mhz
            && local_antenna_mask & !self.antenna_mask == 0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamCasterCapabilities {
    pub observed_at_ms: u64,
    pub model: Option<StreamCasterModel>,
    pub firmware_version: Option<String>,
    pub supported_frequency_profiles: Vec<StreamCasterFrequencyProfile>,
    #[serde(default)]
    pub scheduled_activation_supported: bool,
    #[serde(default)]
    pub dual_profile_supported: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamCasterEffectiveSettings {
    pub observed_at_ms: u64,
    pub node_id: Option<u32>,
    pub system_name: Option<String>,
    pub network_id: String,
    pub center_frequency_mhz: f64,
    pub bandwidth_mhz: ChannelBandwidthMhz,
    pub link_distance_m: u32,
    pub antenna_mask: u8,
    pub transmit_power_dbm_per_port: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamCasterOperationIntent {
    Validate,
    Prepare,
    Activate,
    Rollback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum FleetActivationMechanism {
    ValidationOnly,
    Scheduled { activate_at_ms: u64 },
    IndependentManagement,
}

/// ARC-owned safety facts that authorize a previously prepared transaction.
///
/// Vendor capability and installation evidence remain owned by the radio
/// plugin. These fields are deliberately narrow so the plugin cannot infer
/// aircraft state or control-bearer health on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArcActivationAuthorization {
    pub maintenance_window_authorized: bool,
    pub known_landed: bool,
    pub preserves_control_bearer: bool,
    pub authorized_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamCasterOperationRequest {
    pub schema_version: u16,
    pub request_id: String,
    pub intent: StreamCasterOperationIntent,
    pub fleet_plan: ArcRadioConfiguration,
    pub assignment: StreamCasterDeviceAssignment,
    pub activation: FleetActivationMechanism,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arc_authorization: Option<ArcActivationAuthorization>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_generation: Option<u64>,
}

impl StreamCasterOperationRequest {
    pub fn validate(&self) -> Result<(), StreamCasterControlError> {
        if self.schema_version != STREAMCASTER_CONTROL_SCHEMA_VERSION {
            return Err(StreamCasterControlError::UnsupportedSchemaVersion(
                self.schema_version,
            ));
        }
        validate_identifier("request_id", &self.request_id)?;
        if matches!(self.intent, StreamCasterOperationIntent::Activate) {
            let authorization = self.arc_authorization.ok_or_else(|| {
                StreamCasterControlError::InvalidActivation(
                    "ARC activation authorization is required".into(),
                )
            })?;
            if !authorization.maintenance_window_authorized
                || !authorization.known_landed
                || !authorization.preserves_control_bearer
                || authorization.authorized_at_ms == 0
            {
                return Err(StreamCasterControlError::InvalidActivation(
                    "ARC maintenance, landed, and alternate-control-bearer authorization is required"
                        .into(),
                ));
            }
        }
        self.assignment.validate()?;
        self.fleet_plan.assess()?;
        let matching = self
            .fleet_plan
            .fleet
            .groups
            .iter()
            .find(|group| group.group_id == self.assignment.group_id)
            .ok_or_else(|| {
                StreamCasterControlError::UnknownAssignmentGroup(self.assignment.group_id.clone())
            })?;
        if matching.model != self.assignment.expected_model {
            return Err(StreamCasterControlError::AssignmentModelMismatch);
        }
        if matches!(self.intent, StreamCasterOperationIntent::Activate)
            && matches!(self.activation, FleetActivationMechanism::ValidationOnly)
        {
            return Err(StreamCasterControlError::MissingActivationMechanism);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct StreamCasterActivationGates {
    pub known_landed: bool,
    pub hardware_apply_enabled: bool,
    pub live_capability_match: bool,
    pub regulatory_authorized: bool,
    pub antenna_installation_resolved: bool,
    pub credential_resolved: bool,
    pub rollback_snapshot_staged: bool,
    pub independent_management_reachable: bool,
    pub scheduled_activation_supported: bool,
    pub preserves_control_bearer: bool,
}

impl StreamCasterActivationGates {
    pub fn ready_for_prepare(self) -> bool {
        self.known_landed
            && self.hardware_apply_enabled
            && self.live_capability_match
            && self.regulatory_authorized
            && self.antenna_installation_resolved
            && self.credential_resolved
            && self.rollback_snapshot_staged
            && self.preserves_control_bearer
    }

    pub fn supports(self, mechanism: FleetActivationMechanism) -> bool {
        match mechanism {
            FleetActivationMechanism::ValidationOnly => false,
            FleetActivationMechanism::Scheduled { .. } => self.scheduled_activation_supported,
            FleetActivationMechanism::IndependentManagement => {
                self.independent_management_reachable
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamCasterApplyPhase {
    Validating,
    Blocked,
    Prepared,
    Activating,
    Reconnecting,
    Verifying,
    Effective,
    Drifted,
    Failed,
    RolledBack,
    RecoveryRequired,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamCasterOperationStatus {
    pub schema_version: u16,
    pub request_id: String,
    pub node_id: NodeId,
    pub generation: u64,
    pub observed_at_ms: u64,
    pub phase: StreamCasterApplyPhase,
    pub gates: StreamCasterActivationGates,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<StreamCasterCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective: Option<StreamCasterEffectiveSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetCutoverPhase {
    Draft,
    Distributing,
    Prepared,
    Activating,
    Verifying,
    Effective,
    Aborting,
    RolledBack,
    RecoveryRequired,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetCutoverCoordinator {
    generation: u64,
    required_nodes: BTreeSet<NodeId>,
    phase: FleetCutoverPhase,
    prepared: BTreeMap<NodeId, StreamCasterActivationGates>,
    effective: BTreeSet<NodeId>,
    failed: BTreeMap<NodeId, String>,
}

impl FleetCutoverCoordinator {
    pub fn new(
        generation: u64,
        required_nodes: impl IntoIterator<Item = NodeId>,
    ) -> Result<Self, StreamCasterControlError> {
        let required_nodes: BTreeSet<_> = required_nodes.into_iter().collect();
        if generation == 0 {
            return Err(StreamCasterControlError::InvalidGeneration);
        }
        if required_nodes.is_empty() {
            return Err(StreamCasterControlError::EmptyRequiredNodes);
        }
        Ok(Self {
            generation,
            required_nodes,
            phase: FleetCutoverPhase::Draft,
            prepared: BTreeMap::new(),
            effective: BTreeSet::new(),
            failed: BTreeMap::new(),
        })
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn phase(&self) -> FleetCutoverPhase {
        self.phase
    }

    pub fn distribute(&mut self) -> Result<(), StreamCasterControlError> {
        self.require_phase(FleetCutoverPhase::Draft)?;
        self.phase = FleetCutoverPhase::Distributing;
        Ok(())
    }

    pub fn acknowledge_prepared(
        &mut self,
        node_id: NodeId,
        generation: u64,
        gates: StreamCasterActivationGates,
    ) -> Result<(), StreamCasterControlError> {
        if self.phase != FleetCutoverPhase::Distributing
            && self.phase != FleetCutoverPhase::Prepared
        {
            return Err(StreamCasterControlError::InvalidCutoverTransition {
                from: self.phase,
                action: "acknowledge_prepared",
            });
        }
        self.validate_member_generation(&node_id, generation)?;
        if !gates.ready_for_prepare() {
            return Err(StreamCasterControlError::PreparationGatesFailed(node_id));
        }
        self.prepared.insert(node_id, gates);
        if self.prepared.len() == self.required_nodes.len() {
            self.phase = FleetCutoverPhase::Prepared;
        }
        Ok(())
    }

    pub fn activate(
        &mut self,
        maintenance_window_authorized: bool,
        mechanism: FleetActivationMechanism,
    ) -> Result<(), StreamCasterControlError> {
        self.require_phase(FleetCutoverPhase::Prepared)?;
        if !maintenance_window_authorized {
            return Err(StreamCasterControlError::MaintenanceWindowRequired);
        }
        let unsupported: Vec<_> = self
            .required_nodes
            .iter()
            .filter(|node_id| {
                !self
                    .prepared
                    .get(*node_id)
                    .is_some_and(|gates| gates.supports(mechanism))
            })
            .cloned()
            .collect();
        if !unsupported.is_empty() {
            return Err(StreamCasterControlError::UnsafeActivationMechanism(
                unsupported,
            ));
        }
        self.phase = FleetCutoverPhase::Activating;
        Ok(())
    }

    pub fn begin_verification(&mut self) -> Result<(), StreamCasterControlError> {
        self.require_phase(FleetCutoverPhase::Activating)?;
        self.phase = FleetCutoverPhase::Verifying;
        Ok(())
    }

    pub fn acknowledge_effective(
        &mut self,
        node_id: NodeId,
        generation: u64,
    ) -> Result<(), StreamCasterControlError> {
        self.require_phase(FleetCutoverPhase::Verifying)?;
        self.validate_member_generation(&node_id, generation)?;
        self.effective.insert(node_id);
        if self.effective.len() == self.required_nodes.len() {
            self.phase = FleetCutoverPhase::Effective;
        }
        Ok(())
    }

    pub fn fail(
        &mut self,
        node_id: NodeId,
        generation: u64,
        reason: impl Into<String>,
    ) -> Result<(), StreamCasterControlError> {
        self.validate_member_generation(&node_id, generation)?;
        self.failed.insert(node_id, reason.into());
        self.phase = FleetCutoverPhase::RecoveryRequired;
        Ok(())
    }

    pub fn begin_rollback(&mut self) -> Result<(), StreamCasterControlError> {
        self.require_phase(FleetCutoverPhase::RecoveryRequired)?;
        self.phase = FleetCutoverPhase::Aborting;
        Ok(())
    }

    pub fn complete_rollback(&mut self) -> Result<(), StreamCasterControlError> {
        self.require_phase(FleetCutoverPhase::Aborting)?;
        self.phase = FleetCutoverPhase::RolledBack;
        Ok(())
    }

    fn validate_member_generation(
        &self,
        node_id: &NodeId,
        generation: u64,
    ) -> Result<(), StreamCasterControlError> {
        if generation != self.generation {
            return Err(StreamCasterControlError::StaleGeneration {
                expected: self.generation,
                actual: generation,
            });
        }
        if !self.required_nodes.contains(node_id) {
            return Err(StreamCasterControlError::UnexpectedNode(node_id.clone()));
        }
        Ok(())
    }

    fn require_phase(&self, expected: FleetCutoverPhase) -> Result<(), StreamCasterControlError> {
        if self.phase == expected {
            Ok(())
        } else {
            Err(StreamCasterControlError::InvalidCutoverTransition {
                from: self.phase,
                action: "phase_transition",
            })
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum StreamCasterControlError {
    #[error("unsupported StreamCaster control schema version {0}")]
    UnsupportedSchemaVersion(u16),
    #[error("StreamCaster node ID cannot be empty")]
    EmptyNodeId,
    #[error("invalid {field} identifier {value:?}")]
    InvalidIdentifier { field: &'static str, value: String },
    #[error("invalid management interface {0:?}")]
    InvalidManagementInterface(String),
    #[error("invalid management IP address {0:?}")]
    InvalidManagementAddress(String),
    #[error("invalid {field} reference {value:?}")]
    InvalidReference { field: &'static str, value: String },
    #[error("assigned group {0:?} is not present in the fleet plan")]
    UnknownAssignmentGroup(String),
    #[error("assigned radio model does not match the fleet group")]
    AssignmentModelMismatch,
    #[error("activate intent requires a safe activation mechanism")]
    MissingActivationMechanism,
    #[error("invalid ARC activation authorization: {0}")]
    InvalidActivation(String),
    #[error("fleet cutover generation must be positive")]
    InvalidGeneration,
    #[error("fleet cutover requires at least one required node")]
    EmptyRequiredNodes,
    #[error("invalid fleet cutover transition from {from:?} during {action}")]
    InvalidCutoverTransition {
        from: FleetCutoverPhase,
        action: &'static str,
    },
    #[error("node {0} is not required by this cutover")]
    UnexpectedNode(NodeId),
    #[error("stale generation {actual}; expected {expected}")]
    StaleGeneration { expected: u64, actual: u64 },
    #[error("node {0} did not satisfy every preparation gate")]
    PreparationGatesFailed(NodeId),
    #[error("fleet activation requires an authorized maintenance window")]
    MaintenanceWindowRequired,
    #[error("activation mechanism is unsafe or unsupported for nodes {0:?}")]
    UnsafeActivationMechanism(Vec<NodeId>),
    #[error(transparent)]
    InvalidFleetPlan(#[from] crate::RadioConfigError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_gates() -> StreamCasterActivationGates {
        StreamCasterActivationGates {
            known_landed: true,
            hardware_apply_enabled: true,
            live_capability_match: true,
            regulatory_authorized: true,
            antenna_installation_resolved: true,
            credential_resolved: true,
            rollback_snapshot_staged: true,
            independent_management_reachable: true,
            scheduled_activation_supported: false,
            preserves_control_bearer: true,
        }
    }

    #[test]
    fn device_assignment_rejects_secret_or_absolute_path_shaped_references() {
        let assignment = StreamCasterDeviceAssignment {
            schema_version: STREAMCASTER_CONTROL_SCHEMA_VERSION,
            node_id: NodeId::from("air-001"),
            group_id: "air".into(),
            expected_model: StreamCasterModel::Sl5200LiteEstimated,
            management_interface: "eth1".into(),
            data_interface: "streamcaster0".into(),
            management_address: "192.168.169.11".into(),
            antenna_installation_profile_id: Some("installations/airframe-a".into()),
            credential_reference: Some("/etc/arc/plaintext-password".into()),
            hardware_apply_enabled: false,
        };

        assert!(matches!(
            assignment.validate(),
            Err(StreamCasterControlError::InvalidReference {
                field: "credential_reference",
                ..
            })
        ));
    }

    #[test]
    fn cutover_requires_every_node_and_an_independent_activation_path() {
        let nodes = [NodeId::from("air-001"), NodeId::from("gcs-001")];
        let mut cutover = FleetCutoverCoordinator::new(7, nodes.clone()).unwrap();
        cutover.distribute().unwrap();
        cutover
            .acknowledge_prepared(nodes[0].clone(), 7, ready_gates())
            .unwrap();
        assert_eq!(cutover.phase(), FleetCutoverPhase::Distributing);
        assert!(cutover
            .activate(true, FleetActivationMechanism::IndependentManagement)
            .is_err());

        cutover
            .acknowledge_prepared(nodes[1].clone(), 7, ready_gates())
            .unwrap();
        assert_eq!(cutover.phase(), FleetCutoverPhase::Prepared);
        cutover
            .activate(true, FleetActivationMechanism::IndependentManagement)
            .unwrap();
        cutover.begin_verification().unwrap();
        cutover.acknowledge_effective(nodes[0].clone(), 7).unwrap();
        assert_eq!(cutover.phase(), FleetCutoverPhase::Verifying);
        cutover.acknowledge_effective(nodes[1].clone(), 7).unwrap();
        assert_eq!(cutover.phase(), FleetCutoverPhase::Effective);
    }

    #[test]
    fn cutover_blocks_scheduled_activation_when_any_node_lacks_support() {
        let node = NodeId::from("air-001");
        let mut cutover = FleetCutoverCoordinator::new(8, [node.clone()]).unwrap();
        cutover.distribute().unwrap();
        cutover
            .acknowledge_prepared(node.clone(), 8, ready_gates())
            .unwrap();

        assert_eq!(
            cutover.activate(
                true,
                FleetActivationMechanism::Scheduled {
                    activate_at_ms: 10_000,
                }
            ),
            Err(StreamCasterControlError::UnsafeActivationMechanism(vec![
                node
            ]))
        );
    }

    #[test]
    fn failed_cutover_requires_explicit_rollback() {
        let node = NodeId::from("air-001");
        let mut cutover = FleetCutoverCoordinator::new(9, [node.clone()]).unwrap();
        cutover.distribute().unwrap();
        cutover.fail(node, 9, "radio did not reconnect").unwrap();
        assert_eq!(cutover.phase(), FleetCutoverPhase::RecoveryRequired);
        cutover.begin_rollback().unwrap();
        cutover.complete_rollback().unwrap();
        assert_eq!(cutover.phase(), FleetCutoverPhase::RolledBack);
    }
}
