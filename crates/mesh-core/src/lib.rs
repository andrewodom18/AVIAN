//! Hardware-independent domain types for AVIAN.

mod altitude;
mod command;
mod link;
mod message;
mod node;
mod relay;
mod relay_runtime;
mod topology;
mod traffic;

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
pub use relay::{
    AssignmentPool, GeoPoint, MissionAllocation, OperatorTaskGroup, RadioLinkBudget, RangeEvidence,
    RelayAllocationMode, RelayCandidate, RelayCorridorRequest, RelayCoverage, RelayFeasibility,
    RelayPairBroadcastAction, RelayPairHandoverError, RelayPairHandoverPolicy, RelayPairHeartbeat,
    RelayPeerCoordination, RelayPlan, RelayPlanError, RelayPlanner, RelayPolicy, RelayRangeModel,
    RelayStation, RelayStationTransmission, SILVUS_SL5200_1_25_MHZ_SENSITIVITY_DBM,
    SILVUS_SL5200_5_MHZ_SENSITIVITY_DBM, SILVUS_SL5200_NATIVE_TX_POWER_DBM,
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
