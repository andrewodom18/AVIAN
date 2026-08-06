//! Hardware-independent domain types for AVIAN.

mod altitude;
mod command;
mod link;
mod message;
mod node;
mod radio;
mod radio_control;
mod radio_observation;
mod relay;
mod relay_runtime;
mod topology;
mod traffic;
mod vendor_radio;

pub use altitude::{Altitude, AltitudeError, SYSTEM_MAX_MSL_FT, SYSTEM_MAX_MSL_M};
pub use command::{CommandError, EmergencyAction, EmergencyCommand, ReplayGuard};
pub use link::{
    LinkCandidate, LinkGeometry, LinkId, LinkMetrics, LinkOrchestrator, RoutePlan, TransportKind,
};
pub use message::{
    DeliveryClass, DeliveryPolicy, EmergencyAck, MeshPayload, MissionState, MissionStatus,
    Telemetry,
};
pub use node::{Capability, FlightStack, NodeId, NodeProfile, NodeRole, ProfileError};
pub use radio::{
    ArcRadioConfiguration, ChannelBandwidthMhz, OemDimensionsMm, PriorityTransferAssessment,
    RadioConfigAuthority, RadioConfigError, RadioFleetDefinition, RadioNodeAssignment,
    RadioNodeGroup, RadioNodeRole, RadioPlanAssessment, RadioProfileEvidence,
    RadioRegulatoryProfile, RadioTrafficLoad, RadioTrafficProfile, SilvusApiStep,
    SilvusGroupApplyTemplate, SilvusStepEffect, Sl5200PowerProfile, StreamCasterModel,
    StreamCasterModelProfile, StreamCasterNetworkSettings, StreamCasterOemIntegrationProfile,
    StreamCasterRfBand, TransmitPowerMode, DEFAULT_MAX_AIRTIME_RATIO,
    DEFAULT_PRIORITY_SOURCE_NODES, DEFAULT_PRIORITY_TRANSFER_BYTES,
    DEFAULT_ROUTINE_PACKETS_PER_SECOND, DEFAULT_ROUTINE_PACKET_BYTES,
    FCC_SL52_245_10_MHZ_MAX_CENTER_FREQUENCY_MHZ,
    FCC_SL52_245_10_MHZ_MAX_CONDUCTED_POWER_PER_PORT_DBM,
    FCC_SL52_245_10_MHZ_MIN_CENTER_FREQUENCY_MHZ, FCC_SL52_245_20_MHZ_CENTER_FREQUENCY_MHZ,
    FCC_SL52_245_20_MHZ_MAX_CONDUCTED_POWER_PER_PORT_DBM, MAX_STREAMCASTER_LINK_DISTANCE_M,
    RADIO_CONFIG_SCHEMA_VERSION, RADIO_VALIDATION_TARGET_NODES, SL5200_OEM_INTEGRATION_PROFILE,
};
pub use radio_control::{
    ArcActivationAuthorization, FleetActivationMechanism, FleetCutoverCoordinator,
    FleetCutoverPhase, StreamCasterActivationGates, StreamCasterApplyPhase,
    StreamCasterCapabilities, StreamCasterControlError, StreamCasterDeviceAssignment,
    StreamCasterEffectiveSettings, StreamCasterFrequencyProfile, StreamCasterOperationIntent,
    StreamCasterOperationRequest, StreamCasterOperationStatus, STREAMCASTER_CONTROL_SCHEMA_VERSION,
};
pub use radio_observation::{
    StreamCasterMeshObservation, StreamCasterObservedNode, StreamCasterObservedPosition,
    StreamCasterObservedRadio, StreamCasterObservedStatus, StreamCasterPeerLink,
    StreamCasterRfLink, STREAMCASTER_CAPACITY_REQUIREMENT_NODES,
    STREAMCASTER_MESH_OBSERVATION_SCHEMA_VERSION,
};
pub use relay::{
    AssignmentPool, GeoPoint, MissionAllocation, OperatorTaskGroup, RadioLinkBudget, RangeEvidence,
    RelayAllocationMode, RelayCandidate, RelayCorridorRequest, RelayCoverage, RelayFeasibility,
    RelayPairBroadcastAction, RelayPairHandoverError, RelayPairHandoverPolicy, RelayPairHeartbeat,
    RelayPeerCoordination, RelayPlan, RelayPlanError, RelayPlanner, RelayPolicy, RelayRangeModel,
    RelayStation, RelayStationTransmission, SILVUS_SL5200_1_25_MHZ_SENSITIVITY_DBM,
    SILVUS_SL5200_5_MHZ_SENSITIVITY_DBM, SILVUS_SL5220_PER_PORT_TX_POWER_DBM,
    SILVUS_SL5220_TOTAL_RF_POWER_DBM,
};
pub use relay_runtime::{
    InFlightRelayDecision, InFlightRelayPlanner, InFlightRelayRequest, LiveRelayCandidate,
    RelayAnchor, RelayBroadcastPair, RelayChainHop, RelayChainRoute, RelayHealthPolicy,
    RelayLinkObservation, RelayRoleGroup, RelayRuntimeAction, RelayRuntimeConfigError,
    RelayRuntimeConfiguration, RelayRuntimeError, RelayRuntimeSnapshot, RuntimeRelayAllocationMode,
};
pub use topology::{
    TopologyError, TopologyPlan, TopologyPlanner, DEFAULT_MAX_NEIGHBORS, MAX_SUPPORTED_SWARM_SIZE,
    MIN_SUPPORTED_SWARM_SIZE,
};
pub use traffic::{
    RelayObservationPublication, RelayObservationTrafficGovernor, SwarmStatusSummary,
    SwarmTrafficPolicy, TelemetryPublication, TelemetryTrafficGovernor, TrafficPolicyError,
};
pub use vendor_radio::{
    RadioCapabilities, RadioChannelCapability, RadioDeviceObservation, RadioDeviceStatus,
    RadioEffectiveState, RadioEvidenceLevel, RadioFrequencyRange, RadioIdentity,
    RadioManagementInterface, RadioNeighborObservation, RadioNetworkMode, RadioVendorId,
    VendorRadioError, RADIO_DEVICE_SCHEMA_VERSION,
};
